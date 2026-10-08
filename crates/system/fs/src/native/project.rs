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
//!   removals come last, deepest first;
//! - a pass first prepares every version it will apply (fetching small
//!   files into staging), and applies them only then, paths that are new
//!   here before paths that change, so the tree moves from one state to
//!   the next in one quick step. When a version is not available yet, the
//!   rest of its author's changeset waits with it (`storage.md` §10, V3):
//!   a git ref is never shown with its index still missing.
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
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use fs_native::hydrate::{Hydrator as Placeholders, PlaceholderSpec, STAGING_PREFIX};

use super::state::{LocalStat, Materialized};
use crate::fs::native_ops::TreeEntry;
use crate::hydration::Hydrator;
use crate::namespace::{PosixAttrs, VersionFence};

/// What the projector needs to fence lease-class files.
pub struct Fencing<'a> {
    /// This device.
    pub me: [u8; 32],
    /// Who held a record's lease in an epoch.
    pub holder_of: &'a dyn Fn([u8; 16], u64) -> Option<[u8; 32]>,
    pub now_ms: u64,
}

/// Whether the current version of a lease-class file may replace what is
/// materialised (`storage.md` §11.2, fencing). A version tagged with a
/// tenancy is accepted only from the device that held that epoch, and
/// never from an epoch older than the one already materialised; a version
/// written without a lease is accepted only while nobody holds one. A file
/// without a lease record is not lease-class and always accepted.
pub fn fence_accepts(e: &TreeEntry, materialized: Option<VersionFence>, now_ms: u64) -> bool {
    let Some(lease) = e.lease else {
        return true;
    };
    match e.fence {
        Some(f) => {
            e.fence_holder == Some(f.holder) && materialized.is_none_or(|m| f.epoch >= m.epoch)
        }
        None => !lease.is_live(now_ms),
    }
}

/// Largest symbolic link target (`PATH_MAX`).
const MAX_LINK_TARGET: u64 = 4096;

/// The permission bits of a local path, as the record plane keeps them.
fn local_mode(path: &Path) -> Option<u32> {
    let meta = fs::symlink_metadata(path).ok()?;
    let ft = meta.file_type();
    Some(PosixAttrs::of_local(meta.permissions().mode(), ft.is_dir(), ft.is_symlink()).mode)
}

pub fn rel_path(components: &[Vec<u8>]) -> PathBuf {
    components
        .iter()
        .map(|c| OsString::from_vec(c.clone()))
        .collect()
}

/// Whether `rel` stays inside the tree it is joined onto: at least one
/// component, every one of them a plain name. A `..`, a root or a prefix
/// component would make `live.join(rel)` name something outside `live`,
/// which this plane writes as root.
pub fn confined_rel(rel: &Path) -> bool {
    let mut components = rel.components().peekable();
    components.peek().is_some() && components.all(|c| matches!(c, Component::Normal(_)))
}

#[cfg(test)]
mod confinement_tests {
    use super::{confined_rel, rel_path};

    #[test]
    fn only_plain_relative_paths_are_confined() {
        assert!(confined_rel(&rel_path(&[b"a".to_vec(), b"b.txt".to_vec()])));
        assert!(confined_rel(&rel_path(&[b"..x".to_vec()])));
        for hostile in [
            vec![],
            vec![b"..".to_vec()],
            vec![b"../etc".to_vec()],
            vec![b"a".to_vec(), b"..".to_vec(), b"b".to_vec()],
            vec![b"/etc/cron.d/x".to_vec()],
            vec![b"a/../../b".to_vec()],
        ] {
            assert!(
                !confined_rel(&rel_path(&hostile)),
                "{hostile:?} must not be written through"
            );
        }
    }
}

/// `path` with its `from` prefix replaced by `to`, if it lies below `from`.
pub fn rebase(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    path.strip_prefix(from).ok().map(|rest| {
        if rest.as_os_str().is_empty() {
            to.to_path_buf()
        } else {
            to.join(rest)
        }
    })
}

/// Moves the entries of `from` and everything below it to `to`. Returns
/// each moved entry's old path, new path and record.
pub fn rename_entries<S: std::hash::BuildHasher>(
    map: &mut HashMap<PathBuf, Materialized, S>,
    from: &Path,
    to: &Path,
) -> Vec<(PathBuf, PathBuf, Materialized)> {
    let old: Vec<PathBuf> = map
        .keys()
        .filter(|p| p.starts_with(from))
        .cloned()
        .collect();
    let mut out = Vec::with_capacity(old.len());
    for p in old {
        let (Some(record), Some(new)) = (map.remove(&p), rebase(&p, from, to)) else {
            continue;
        };
        out.push((p, new, record));
    }
    for (_, new, record) in &out {
        map.insert(new.clone(), record.clone());
    }
    out
}

fn record_key(e: &TreeEntry) -> Option<[u8; 16]> {
    e.record.map(|r| *r.0.as_bytes())
}

/// How long a pass holds a changeset back for one version that does not
/// arrive, before applying what it can in version order.
const HOLD_LIMIT: Duration = Duration::from_secs(60);

/// Directory at the room's root where versions are prepared before they
/// are renamed into place. Its name, like every staging name, is never
/// captured.
pub const STAGING_AREA: &str = ".tilo-staging-area";

/// How a prepared version reaches its path.
enum Install {
    /// A staged file or link, renamed into place.
    Staged(PathBuf),
    Placeholder {
        spec: Box<PlaceholderSpec>,
        replace: bool,
    },
    /// Same content, new mode bits.
    Chmod(u32),
}

/// One version prepared for the commit step.
struct Pending {
    rel: PathBuf,
    /// Nothing is at the path yet.
    new: bool,
    mtime_ms: u64,
    install: Install,
    record: Materialized,
}

enum Prepared {
    Done,
    Pending(Box<Pending>),
    /// Its bytes are not reachable right now.
    Unavailable,
}

/// What one projection pass did.
#[derive(Debug, Default)]
pub struct Projection {
    /// Paths written or created (files, placeholders, directories).
    pub written: Vec<PathBuf>,
    /// Paths removed from the live tree.
    pub removed: Vec<PathBuf>,
    /// Local renames that followed a record's move (old path, new path).
    pub moved: Vec<(PathBuf, PathBuf)>,
    /// Versions refused by lease fencing; the local file stays.
    pub fenced: Vec<PathBuf>,
    /// Fenced files this device holds the lease of: it publishes its own
    /// content again, so the holder's version is the head.
    pub reassert: Vec<PathBuf>,
    /// Conflict copies this device made of its own fenced versions before
    /// taking the holder's; the next epoch publishes them.
    pub conflicts: Vec<PathBuf>,
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
    /// Write gates of lease-class files. A new version of a gated file is
    /// gated before it is renamed into place (a placeholder right after it
    /// is created, while it is still a hole), so no writer gets through
    /// ungated.
    write_gates: Arc<super::lease::WriteGates>,
    last_unavailable: std::sync::Mutex<Option<String>>,
    /// The version the last pass stopped at, and since when.
    held: std::sync::Mutex<Option<(PathBuf, u64, std::time::Instant)>>,
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
        write_gates: Arc<super::lease::WriteGates>,
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
            write_gates,
            last_unavailable: std::sync::Mutex::new(None),
            held: std::sync::Mutex::new(None),
        }
    }

    fn chown(&self, path: &Path) -> io::Result<()> {
        if let Some((uid, gid)) = self.owner {
            std::os::unix::fs::lchown(path, Some(uid), Some(gid))?;
        }
        Ok(())
    }

    /// Brings the live tree to `tree`, the record plane's current state.
    /// `changesets` are the version-time spans of other devices' latest
    /// changesets: a version that cannot be applied yet holds back the rest
    /// of the changeset it belongs to.
    pub fn project(
        &self,
        tree: &[TreeEntry],
        materialized: &HashMap<PathBuf, Materialized>,
        changesets: &[(u64, u64)],
        fencing: &Fencing<'_>,
    ) -> Projection {
        let mut out = Projection::default();
        let desired: HashSet<PathBuf> = tree.iter().map(|e| rel_path(&e.path)).collect();
        let moved = self.project_moves(tree, materialized, &desired, &mut out);
        let materialized = moved.as_ref().unwrap_or(materialized);

        let mut dirs: Vec<&TreeEntry> = tree.iter().filter(|e| e.dir).collect();
        dirs.sort_by_key(|e| e.path.len());
        for e in dirs {
            let rel = rel_path(&e.path);
            if !e.known {
                // Placed before the changeset that made it arrived; it
                // appears with that changeset.
                out.waiting = true;
                out.waiting_for.get_or_insert_with(|| rel.clone());
                continue;
            }
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
        let mut pending: Vec<Pending> = Vec::new();
        let mut stopped_at: Option<(PathBuf, u64)> = None;
        self.prefetch_eager(&files, materialized);
        for e in files {
            let rel = rel_path(&e.path);
            if !e.known {
                // Content revisions arrive per record, in no particular
                // order. Nothing made after a version this device cannot
                // verify yet is shown before it: the next pass continues.
                stopped_at = Some((rel, e.mtime_ms));
                break;
            }
            match self.prepare_file(&rel, e, materialized, fencing, &mut out) {
                Ok(Prepared::Done) => {}
                Ok(Prepared::Pending(p)) => pending.push(*p),
                Ok(Prepared::Unavailable) => {
                    // Its bytes are not reachable right now; what follows it
                    // waits, as for an unverified version.
                    stopped_at = Some((rel, e.mtime_ms));
                    break;
                }
                Err(error) => out.errors.push(format!("{}: {error}", rel.display())),
            }
        }
        if let Some((rel, mtime_ms)) = stopped_at {
            let cut = self.hold_cut(&rel, mtime_ms, changesets);
            let (apply, hold): (Vec<Pending>, Vec<Pending>) =
                pending.into_iter().partition(|p| p.mtime_ms < cut);
            Self::discard(hold);
            self.commit(apply, &mut out);
            out.waiting = true;
            out.waiting_for = Some(rel);
            return out;
        }
        if let Ok(mut held) = self.held.lock() {
            *held = None;
        }
        self.commit(pending, &mut out);

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

    /// Renames local paths whose record the tree now places elsewhere, so
    /// a moved file keeps its inode, its resident bytes and its open
    /// readers, and a moved directory takes its subtree along. Returns the
    /// materialised table as the renames left it, when there were any.
    ///
    /// A path changed locally since the daemon wrote it is never moved: it
    /// is captured as the local change it is, and the moved record arrives
    /// as a new file.
    fn project_moves(
        &self,
        tree: &[TreeEntry],
        materialized: &HashMap<PathBuf, Materialized>,
        desired: &HashSet<PathBuf>,
        out: &mut Projection,
    ) -> Option<HashMap<PathBuf, Materialized>> {
        let mut wanted: Vec<(PathBuf, [u8; 16])> = tree
            .iter()
            .filter_map(|e| Some((rel_path(&e.path), record_key(e)?)))
            .collect();
        // Outermost first: a directory's move carries its descendants.
        wanted.sort_by_key(|(p, _)| p.components().count());
        let mut at: HashMap<[u8; 16], PathBuf> = materialized
            .iter()
            .filter_map(|(p, m)| Some((m.record?, p.clone())))
            .collect();
        let mut table: Option<HashMap<PathBuf, Materialized>> = None;
        for (to, record) in wanted {
            let Some(from) = at
                .get(&record)
                .filter(|p| **p != to && !desired.contains(*p))
            else {
                continue;
            };
            let from = from.clone();
            let current = table.as_ref().unwrap_or(materialized);
            let Some(m) = current.get(&from).cloned() else {
                continue;
            };
            let src = self.live.join(&from);
            let dst = self.live.join(&to);
            let ours = if m.dir {
                fs::symlink_metadata(&src).is_ok_and(|meta| meta.is_dir())
            } else {
                m.local.is_some() && m.local == LocalStat::of(&src)
            };
            if !ours || fs::symlink_metadata(&dst).is_ok() {
                continue;
            }
            if let Some(parent) = dst.parent()
                && let Err(error) = self.make_dirs(parent)
            {
                out.errors.push(format!("{}: {error}", to.display()));
                continue;
            }
            if let Err(error) = fs::rename(&src, &dst) {
                out.errors.push(format!(
                    "moving {} to {}: {error}",
                    from.display(),
                    to.display()
                ));
                continue;
            }
            let table = table.get_or_insert_with(|| materialized.clone());
            for (old, new, record) in rename_entries(table, &from, &to) {
                if let Some(r) = record.record {
                    at.insert(r, new.clone());
                }
                out.updates.push((old, None));
                out.updates.push((new, Some(record)));
            }
            out.moved.push((from, to));
        }
        table
    }

    /// Creates `abs` and any missing ancestors inside the live tree as the
    /// daemon creates directories.
    fn make_dirs(&self, abs: &Path) -> io::Result<()> {
        if abs.is_dir() {
            return Ok(());
        }
        if let Some(parent) = abs.parent()
            && parent.starts_with(&self.live)
            && parent != self.live
        {
            self.make_dirs(parent)?;
        }
        self.make_dir(abs)
    }

    /// Fetches the content of every small file this pass is about to write,
    /// in a few batched requests, before writing them one by one: many small
    /// files then cost a few round trips, not one each. Stops at the first
    /// version not verified yet, like the pass itself.
    fn prefetch_eager(&self, files: &[&TreeEntry], materialized: &HashMap<PathBuf, Materialized>) {
        /// Bytes prefetched per pass; the rest follow in the next one.
        const PREFETCH_BYTES: u64 = 256 * 1024 * 1024;
        let mut wanted = Vec::new();
        let mut total = 0_u64;
        for e in files {
            if !e.known {
                break;
            }
            let Some(hash) = e.hash.filter(|_| e.size > 0 && e.size <= self.eager_bytes) else {
                continue;
            };
            let current = materialized
                .get(&rel_path(&e.path))
                .is_some_and(|m| !m.dir && m.hash == Some(*hash.as_bytes()) && m.size == e.size);
            if current {
                continue;
            }
            total = total.saturating_add(e.size);
            if total > PREFETCH_BYTES {
                break;
            }
            wanted.push((hash, e.size));
        }
        if wanted.len() < 2 {
            return;
        }
        let hydrator = Arc::clone(&self.content);
        let (deadline, limit) = (self.eager_deadline, self.eager_bytes);
        let _ = self.runtime.block_on(async move {
            tokio::time::timeout(deadline, hydrator.prefetch_files(&wanted, limit)).await
        });
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
            mtime_ms: e.mtime_ms,
            record: record_key(e),
            mode: Some(e.posix.mode),
            ..Materialized::default()
        };
        match fs::symlink_metadata(&abs) {
            Ok(meta) if meta.is_dir() => {
                let previous = materialized.get(rel);
                let now = local_mode(&abs);
                if now != Some(e.posix.mode) {
                    // Only a mode the daemon set (or published) is changed;
                    // a local chmod not captured yet wins.
                    let ours = previous.is_some_and(|m| m.mode.is_none() || m.mode == now);
                    if !ours {
                        out.skipped_dirty.push(rel.to_path_buf());
                        return Ok(());
                    }
                    fs::set_permissions(&abs, fs::Permissions::from_mode(e.posix.mode))?;
                    out.written.push(rel.to_path_buf());
                }
                if previous != Some(&record) {
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
                self.make_dir_as(&abs, e.posix.mode)?;
                out.written.push(rel.to_path_buf());
                out.updates.push((rel.to_path_buf(), Some(record)));
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if materialized.get(rel).is_some_and(|m| m.dir) {
                    // The daemon made it and it is gone: removed or renamed
                    // here, and the next epoch publishes that.
                    out.skipped_dirty.push(rel.to_path_buf());
                    return Ok(());
                }
                if let Some(parent) = abs.parent() {
                    self.make_dirs(parent)?;
                }
                self.make_dir_as(&abs, e.posix.mode)?;
                out.written.push(rel.to_path_buf());
                out.updates.push((rel.to_path_buf(), Some(record)));
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn make_dir(&self, abs: &Path) -> io::Result<()> {
        self.make_dir_as(abs, crate::namespace::DEFAULT_DIR_MODE)
    }

    fn make_dir_as(&self, abs: &Path, mode: u32) -> io::Result<()> {
        match fs::create_dir(abs) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
            Err(e) => return Err(e),
        }
        self.chown(abs)?;
        // After the chown, which clears setgid.
        fs::set_permissions(abs, fs::Permissions::from_mode(mode))
    }

    /// From which version time on the pass waits, when it stopped at `rel`
    /// (version time `mtime_ms`): the start of the changeset that version
    /// belongs to, or the version itself when no known changeset spans it
    /// or it has been waited for too long.
    fn hold_cut(&self, rel: &Path, mtime_ms: u64, changesets: &[(u64, u64)]) -> u64 {
        let since = {
            let Ok(mut held) = self.held.lock() else {
                return mtime_ms;
            };
            match held.as_ref() {
                Some((p, t, since)) if p == rel && *t == mtime_ms => *since,
                _ => {
                    let now = std::time::Instant::now();
                    *held = Some((rel.to_path_buf(), mtime_ms, now));
                    now
                }
            }
        };
        if since.elapsed() >= HOLD_LIMIT {
            return mtime_ms;
        }
        changesets
            .iter()
            .filter(|(first, last)| (*first..=*last).contains(&mtime_ms))
            .map(|(first, _)| *first)
            .min()
            .unwrap_or(mtime_ms)
            .min(mtime_ms)
    }

    /// Applies prepared versions: paths new here first, then changed ones,
    /// each group in version order.
    fn commit(&self, pending: Vec<Pending>, out: &mut Projection) {
        let (new, changed): (Vec<Pending>, Vec<Pending>) = pending.into_iter().partition(|p| p.new);
        for p in new.into_iter().chain(changed) {
            self.install(p, out);
        }
    }

    fn install(&self, p: Pending, out: &mut Projection) {
        let Pending {
            rel,
            install,
            mut record,
            ..
        } = p;
        // `tree_snapshot` already drops names that are not one component;
        // this is the last line before a root write, so it checks again.
        if !confined_rel(&rel) {
            if let Install::Staged(staging) = &install {
                let _ = fs::remove_file(staging);
            }
            out.errors.push(format!(
                "{}: refusing a path that leaves the live tree",
                rel.display()
            ));
            return;
        }
        let abs = self.live.join(&rel);
        let result = (|| -> io::Result<()> {
            if let Some(parent) = abs.parent() {
                self.make_dirs(parent)?;
            }
            match &install {
                Install::Staged(staging) => {
                    if !record.symlink {
                        self.write_gates.gate_new_version(
                            &self.placeholders,
                            staging,
                            &abs,
                            &rel,
                        )?;
                    }
                    if record.symlink && fs::symlink_metadata(&abs).is_ok_and(|m| m.is_dir()) {
                        return Err(io::Error::from_raw_os_error(libc::EISDIR));
                    }
                    fs::rename(staging, &abs)
                }
                Install::Placeholder { spec, replace } => {
                    if *replace {
                        self.placeholders.replace_with_placeholder(&abs, spec)?;
                    } else {
                        self.placeholders.create_placeholder(&abs, spec)?;
                    }
                    self.write_gates
                        .gate_new_version(&self.placeholders, &abs, &abs, &rel)
                }
                Install::Chmod(mode) => {
                    fs::set_permissions(&abs, fs::Permissions::from_mode(*mode))
                }
            }
        })();
        if let Err(error) = result {
            if let Install::Staged(staging) = &install {
                let _ = fs::remove_file(staging);
            }
            out.errors.push(format!("{}: {error}", rel.display()));
            return;
        }
        match install {
            Install::Placeholder { .. } => {
                out.placeholders = out.placeholders.saturating_add(1);
            }
            Install::Staged(_) => out.files = out.files.saturating_add(1),
            Install::Chmod(_) => {}
        }
        record.local = LocalStat::of(&abs);
        out.written.push(rel.clone());
        out.updates.push((rel, Some(record)));
    }

    /// Drops prepared versions that are held back.
    fn discard(pending: Vec<Pending>) {
        for p in pending {
            if let Install::Staged(staging) = p.install {
                let _ = fs::remove_file(staging);
            }
        }
    }

    /// Keeps a copy of this device's own fenced version of `rel` next to
    /// it, named after the tenancy it lost, before the holder's version
    /// replaces it.
    fn save_conflict(&self, rel: &Path, fence: VersionFence) -> io::Result<PathBuf> {
        let name = rel
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let copy = rel.with_file_name(format!(
            "{name}.sync-conflict-{}-e{}",
            hex::encode(&fence.holder[..4]),
            fence.epoch
        ));
        let src = self.live.join(rel);
        let dst = self.live.join(&copy);
        let staging = self.staging_path()?;
        let result = (|| {
            fs::copy(&src, &staging)?;
            self.chown(&staging)?;
            fs::rename(&staging, &dst)
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&staging);
            return Err(error);
        }
        Ok(copy)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "fencing, the dirty check and staging decide one version together"
    )]
    fn prepare_file(
        &self,
        rel: &Path,
        e: &TreeEntry,
        materialized: &HashMap<PathBuf, Materialized>,
        fencing: &Fencing<'_>,
        out: &mut Projection,
    ) -> io::Result<Prepared> {
        let hash = e.hash.map(|h| *h.as_bytes());
        let previous = materialized.get(rel);
        if let Some(m) = previous
            && !m.dir
            && m.hash == hash
            && m.size == e.size
            && m.symlink == e.posix.symlink
        {
            let mut next = Materialized {
                record: record_key(e),
                ..m.clone()
            };
            if !e.posix.symlink && m.mode != Some(e.posix.mode) {
                // Same bytes, new mode: change it in place unless the file
                // changed here since the daemon last wrote it.
                let abs = self.live.join(rel);
                let untouched = m.local.is_some() && m.local == LocalStat::of(&abs);
                let mode_ours = m.mode.is_none() || m.mode == local_mode(&abs);
                if !(untouched && mode_ours) {
                    out.skipped_dirty.push(rel.to_path_buf());
                    return Ok(Prepared::Done);
                }
                next.mode = Some(e.posix.mode);
                return Ok(Prepared::Pending(Box::new(Pending {
                    rel: rel.to_path_buf(),
                    new: false,
                    mtime_ms: e.mtime_ms,
                    install: Install::Chmod(e.posix.mode),
                    record: next,
                })));
            }
            if next != *m {
                out.updates.push((rel.to_path_buf(), Some(next)));
            }
            return Ok(Prepared::Done);
        }
        let abs = self.live.join(rel);
        let local = LocalStat::of(&abs);
        if e.lease.is_some() {
            if !fence_accepts(e, previous.and_then(|m| m.fence), fencing.now_ms) {
                // Never applied and never dropped: the version stays in the
                // record's history, the local file stays as it is.
                out.fenced.push(rel.to_path_buf());
                if e.lease.is_some_and(|l| l.holder == fencing.me) {
                    out.reassert.push(rel.to_path_buf());
                }
                if let Some(m) = previous {
                    out.updates.push((
                        rel.to_path_buf(),
                        Some(Materialized {
                            hash,
                            size: e.size,
                            ..m.clone()
                        }),
                    ));
                }
                return Ok(Prepared::Done);
            }
            // This device's own version lost its tenancy: keep it beside
            // the holder's.
            if let Some(m) = previous
                && m.own
                && let Some(f) = m.fence
                && f.holder == fencing.me
                && m.record
                    .is_some_and(|r| (fencing.holder_of)(r, f.epoch) != Some(fencing.me))
                && m.local.is_some()
                && m.local == local
            {
                match self.save_conflict(rel, f) {
                    Ok(copy) => out.conflicts.push(copy),
                    Err(error) => {
                        out.errors.push(format!(
                            "{}: keeping a conflict copy: {error}",
                            rel.display()
                        ));
                        out.skipped_dirty.push(rel.to_path_buf());
                        return Ok(Prepared::Done);
                    }
                }
            }
        }
        let dirty = match (local, previous) {
            (None, _) => false,
            (Some(now), Some(m)) => m.local != Some(now),
            // A local file the daemon never wrote: the capture publishes it
            // (or finds it identical) before anything replaces it.
            (Some(_), None) => true,
        };
        if dirty {
            out.skipped_dirty.push(rel.to_path_buf());
            return Ok(Prepared::Done);
        }
        let Some(install) = self.prepare_content(hash, e, local.is_some())? else {
            return Ok(Prepared::Unavailable);
        };
        let placeholder = matches!(install, Install::Placeholder { .. });
        Ok(Prepared::Pending(Box::new(Pending {
            rel: rel.to_path_buf(),
            new: local.is_none(),
            mtime_ms: e.mtime_ms,
            install,
            record: Materialized {
                dir: false,
                hash,
                size: e.size,
                mtime_ms: e.mtime_ms,
                placeholder,
                local: None,
                record: record_key(e),
                mode: Some(e.posix.mode),
                symlink: e.posix.symlink,
                fence: e.fence,
                own: false,
            },
        })))
    }

    /// Prepares one version off to the side: a small file or a link is
    /// fetched and staged, a large file becomes a placeholder when it is
    /// installed. `None` when a small file's bytes could not be fetched: a
    /// small file only appears complete.
    fn prepare_content(
        &self,
        hash: Option<[u8; 32]>,
        e: &TreeEntry,
        replace: bool,
    ) -> io::Result<Option<Install>> {
        let Some(content) = hash.filter(|_| e.size > 0) else {
            if e.posix.symlink {
                // A link needs a target; an empty one is not one.
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
            return self
                .stage_file(&[], e.mtime_ms, e.posix.mode)
                .map(|s| Some(Install::Staged(s)));
        };
        if e.posix.symlink && e.size > MAX_LINK_TARGET {
            return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
        }
        if e.size <= self.eager_bytes || e.posix.symlink {
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
                    let staged = if e.posix.symlink {
                        self.stage_link(&bytes, e.mtime_ms)?
                    } else {
                        self.stage_file(&bytes, e.mtime_ms, e.posix.mode)?
                    };
                    Ok(Some(Install::Staged(staged)))
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
            mode: e.posix.mode,
            mtime: mtime_of(e.mtime_ms),
            owner: self.owner,
        };
        Ok(Some(Install::Placeholder {
            spec: Box::new(spec),
            replace,
        }))
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

    /// A fresh name in the staging area, which exists afterwards.
    fn staging_path(&self) -> io::Result<PathBuf> {
        let area = self.live.join(STAGING_AREA);
        if !area.is_dir() {
            match fs::create_dir(&area) {
                Ok(()) => fs::set_permissions(&area, fs::Permissions::from_mode(0o700))?,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        Ok(area.join(format!("{STAGING_PREFIX}{}-{n}", std::process::id())))
    }

    /// Stages a symbolic link to `target`, owned by the local person, with
    /// the version's mtime.
    fn stage_link(&self, target: &[u8], mtime_ms: u64) -> io::Result<PathBuf> {
        use std::os::unix::ffi::OsStrExt as _;
        let staging = self.staging_path()?;
        let result = (|| {
            std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), &staging)?;
            if let Some((uid, gid)) = self.owner {
                std::os::unix::fs::lchown(&staging, Some(uid), Some(gid))?;
            }
            set_link_mtime(&staging, mtime_ms)
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&staging);
            return Err(error);
        }
        Ok(staging)
    }

    /// Stages a complete file with the version's bytes, mode and mtime,
    /// owned by the local person.
    fn stage_file(&self, bytes: &[u8], mtime_ms: u64, mode: u32) -> io::Result<PathBuf> {
        let staging = self.staging_path()?;
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&staging)?;
            file.write_all(bytes)?;
            if let Some((uid, gid)) = self.owner {
                std::os::unix::fs::fchown(&file, Some(uid), Some(gid))?;
            }
            file.set_permissions(fs::Permissions::from_mode(mode))?;
            file.set_modified(mtime_of(mtime_ms))
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&staging);
            return Err(error);
        }
        Ok(staging)
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

/// Sets a symbolic link's own mtime (not its target's).
fn set_link_mtime(path: &Path, mtime_ms: u64) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let secs = i64::try_from(mtime_ms / 1000).unwrap_or(i64::MAX);
    let nanos = i64::try_from((mtime_ms % 1000).saturating_mul(1_000_000)).unwrap_or(0);
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        libc::timespec {
            tv_sec: secs,
            tv_nsec: nanos,
        },
    ];
    // SAFETY: `c` is a valid NUL-terminated path and `times` holds two
    // timespecs, as utimensat requires.
    let rc = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
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
