//! Capture epochs: an atomic snapshot of a room, compared against the
//! previous epoch's snapshot, producing one ordered changeset.
//!
//! Writers are never paused. The snapshot is the crash image of the room at
//! one instant (V2 in `docs/architecture/storage.md` §10), and every byte
//! that is hashed is read from it, never from the live file. Only records the
//! journal names are examined, and within a changed file only the extents
//! that differ from the previous snapshot are re-chunked.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::btrfs;
use crate::chunking::{ChunkStats, Manifest};
use crate::extents;
use crate::hydrate::RESIDENCY_XATTR;
use crate::journal::{JournalBatch, JournalOp};
use crate::sys;

/// Directory marker that opts a tree out of sync, per
/// <https://bford.info/cachedir/>.
const CACHEDIR_TAG: &str = "CACHEDIR.TAG";
const CACHEDIR_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";
/// Device-local marker: a file or directory carrying this xattr (any value)
/// is derived, per-device state and is never synced.
pub const LOCAL_ONLY_XATTR: &str = "user.tilo.local";

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NodeKind {
    File,
    Dir,
    Symlink(PathBuf),
}

/// The replicated state of one path, as captured in an epoch.
#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    /// Permission bits (`0o7777`, setuid excluded).
    pub mode: u32,
    pub size: u64,
    pub mtime_ns: i128,
    pub manifest: Option<Arc<Manifest>>,
    ctime_ns: i128,
    ino: u64,
}

impl Node {
    fn from_metadata(meta: &fs::Metadata, kind: NodeKind) -> Self {
        let ns = |s: i64, n: i64| {
            i128::from(s)
                .saturating_mul(1_000_000_000)
                .saturating_add(i128::from(n))
        };
        Self {
            kind,
            mode: meta.mode() & 0o7777 & !0o4000,
            size: meta.len(),
            mtime_ns: ns(meta.mtime(), meta.mtime_nsec()),
            ctime_ns: ns(meta.ctime(), meta.ctime_nsec()),
            ino: meta.ino(),
            manifest: None,
        }
    }

    /// Whether a peer would see any difference between the two.
    fn same_replicated(&self, other: &Self) -> bool {
        if self.kind != other.kind || self.mode != other.mode {
            return false;
        }
        match self.kind {
            // Directory mtimes move with every child; they are not published.
            NodeKind::Dir | NodeKind::Symlink(_) => true,
            NodeKind::File => {
                self.size == other.size
                    && self.mtime_ns == other.mtime_ns
                    && self.manifest.as_ref().map(|m| m.content_id)
                        == other.manifest.as_ref().map(|m| m.content_id)
            }
        }
    }

    /// Whether nothing about the inode changed at all since `base`.
    fn untouched_since(&self, base: &Self) -> bool {
        self.ino == base.ino
            && self.size == base.size
            && self.mtime_ns == base.mtime_ns
            && self.ctime_ns == base.ctime_ns
    }
}

#[derive(Clone, Debug)]
pub enum ChangeOp {
    Upsert { path: PathBuf, node: Node },
    Remove { path: PathBuf, is_dir: bool },
}

impl ChangeOp {
    pub fn path(&self) -> &Path {
        match self {
            Self::Upsert { path, .. } | Self::Remove { path, .. } => path,
        }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct CaptureStats {
    /// Time to take the atomic snapshot (the only step with any effect on
    /// writers: one Btrfs transaction commit).
    pub snapshot: Duration,
    /// Time to decide what changed.
    pub scan: Duration,
    /// Time spent comparing extents and hashing.
    pub hash: Duration,
    pub total: Duration,
    pub paths_examined: u64,
    pub files_hashed: u64,
    pub full_rescan: bool,
    pub chunks: ChunkStats,
}

/// One device's changes from one epoch, in the order peers apply them.
#[derive(Clone, Debug)]
pub struct Changeset {
    pub epoch: u64,
    /// Journal position the changeset covers.
    pub through: u64,
    pub ops: Vec<ChangeOp>,
    /// Placeholder or partially resident files that were written locally.
    /// Their content can only be captured once they are fully resident.
    pub deferred: Vec<PathBuf>,
    pub stats: CaptureStats,
}

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    /// The room subvolume.
    pub live: PathBuf,
    /// Where epoch snapshots are created; same filesystem as `live`.
    pub epochs: PathBuf,
    pub hash_threads: usize,
}

impl CaptureConfig {
    pub fn new(live: impl Into<PathBuf>, epochs: impl Into<PathBuf>) -> Self {
        Self {
            live: live.into(),
            epochs: epochs.into(),
            hash_threads: std::thread::available_parallelism().map_or(2, |n| n.get().min(8)),
        }
    }
}

struct EpochState {
    snapshot: PathBuf,
    nodes: BTreeMap<PathBuf, Node>,
}

/// Cuts epochs of one room.
pub struct EpochCapture {
    cfg: CaptureConfig,
    prev: Option<EpochState>,
    next_epoch: u64,
}

struct Classified {
    upserts: Vec<(PathBuf, Node)>,
    removes: Vec<(PathBuf, bool)>,
    jobs: Vec<HashJob>,
    deferred: Vec<PathBuf>,
    examined: u64,
}

struct HashJob {
    path: PathBuf,
    node: Node,
    /// The previous version to diff against: its snapshot file and manifest.
    base: Option<(PathBuf, Arc<Manifest>)>,
}

impl EpochCapture {
    pub fn new(cfg: CaptureConfig) -> io::Result<Self> {
        fs::create_dir_all(&cfg.epochs)?;
        Ok(Self {
            cfg,
            prev: None,
            next_epoch: 1,
        })
    }

    /// Captured state of every synced path after the last epoch.
    pub fn nodes(&self) -> impl Iterator<Item = (&Path, &Node)> {
        self.prev
            .iter()
            .flat_map(|p| p.nodes.iter().map(|(k, v)| (k.as_path(), v)))
    }

    /// The snapshot of the last epoch, while it is retained.
    pub fn last_snapshot(&self) -> Option<&Path> {
        self.prev.as_ref().map(|p| p.snapshot.as_path())
    }

    /// Cuts one epoch. Without a previous epoch, or when `batch` asks for a
    /// rescan, the whole room is compared; otherwise only what the journal
    /// names.
    pub fn capture(&mut self, batch: &JournalBatch) -> io::Result<Changeset> {
        let t0 = Instant::now();
        let epoch = self.next_epoch;
        let snapshot = self.cfg.epochs.join(format!("{epoch:016}"));
        btrfs::snapshot(&self.cfg.live, &snapshot, true)?;
        self.next_epoch = self.next_epoch.saturating_add(1);
        let t_snap = t0.elapsed();

        let full = self.prev.is_none() || batch.rescan.is_some();
        let (candidates, order) = self.candidates(&snapshot, batch, full)?;

        let empty = BTreeMap::new();
        let prev_nodes = self.prev.as_ref().map_or(&empty, |p| &p.nodes);
        let Classified {
            mut upserts,
            removes,
            jobs,
            deferred,
            examined,
        } = self.classify(&snapshot, candidates, full)?;
        let t_scan = t0.elapsed();

        let (hashed, chunk_stats) = run_jobs(jobs, self.cfg.hash_threads.max(1), &snapshot)?;
        let files_hashed = hashed.len() as u64;
        upserts.extend(hashed);
        let t_hash = t0.elapsed();

        // Drop no-op upserts, then order everything.
        upserts.retain(|(p, n)| prev_nodes.get(p).is_none_or(|old| !old.same_replicated(n)));
        let ops = order_ops(upserts, removes, &order, full);
        let new_nodes = next_state(prev_nodes, &ops, full, &snapshot);
        if let Some(old) = self.prev.take() {
            btrfs::delete_subvolume(&old.snapshot)?;
        }
        self.prev = Some(EpochState {
            snapshot,
            nodes: new_nodes,
        });
        Ok(Changeset {
            epoch,
            through: batch.through,
            ops,
            deferred,
            stats: CaptureStats {
                snapshot: t_snap,
                scan: t_scan.saturating_sub(t_snap),
                hash: t_hash.saturating_sub(t_scan),
                total: t0.elapsed(),
                paths_examined: examined,
                files_hashed,
                full_rescan: full,
                chunks: chunk_stats,
            },
        })
    }

    /// Sorts candidates into directory/symlink upserts, removals, files to
    /// hash and deferred placeholders.
    fn classify(
        &self,
        snapshot: &Path,
        candidates: HashSet<PathBuf>,
        full: bool,
    ) -> io::Result<Classified> {
        let mut local_only = LocalOnly::new(snapshot);
        let empty = BTreeMap::new();
        let prev_nodes = self.prev.as_ref().map_or(&empty, |p| &p.nodes);
        let prev_snapshot = self.prev.as_ref().map(|p| p.snapshot.clone());
        let by_ino: HashMap<u64, &PathBuf> = if full {
            HashMap::new()
        } else {
            // Renamed files keep their inode; diff them against the old path.
            prev_nodes
                .iter()
                .filter(|(_, n)| n.kind == NodeKind::File)
                .map(|(p, n)| (n.ino, p))
                .collect()
        };

        let mut upserts: Vec<(PathBuf, Node)> = Vec::new();
        let mut removes: Vec<(PathBuf, bool)> = Vec::new();
        let mut jobs: Vec<HashJob> = Vec::new();
        let mut deferred = Vec::new();
        let mut examined = 0u64;
        for path in candidates {
            examined = examined.saturating_add(1);
            let abs = snapshot.join(&path);
            let meta = match fs::symlink_metadata(&abs) {
                Ok(m) => Some(m),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            };
            let synced = match &meta {
                Some(m) => {
                    let ft = m.file_type();
                    let special = ft.is_socket()
                        || ft.is_fifo()
                        || ft.is_block_device()
                        || ft.is_char_device();
                    !special && !local_only.contains(&path)
                }
                None => false,
            };
            let (Some(meta), true) = (meta, synced) else {
                if let Some(old) = prev_nodes.get(&path) {
                    removes.push((path, old.kind == NodeKind::Dir));
                }
                continue;
            };
            let ft = meta.file_type();
            if ft.is_dir() {
                upserts.push((path, Node::from_metadata(&meta, NodeKind::Dir)));
            } else if ft.is_symlink() {
                let target = fs::read_link(&abs)?;
                upserts.push((path, Node::from_metadata(&meta, NodeKind::Symlink(target))));
            } else {
                let node = Node::from_metadata(&meta, NodeKind::File);
                if is_placeholder(&abs) {
                    // Its content is fixed by the version it was created
                    // from. The journal naming it means a local write.
                    if !full {
                        deferred.push(path);
                    }
                    continue;
                }
                let base_path = match prev_nodes.get(&path) {
                    Some(n) if n.kind == NodeKind::File => Some(&path),
                    _ => by_ino.get(&node.ino).copied(),
                };
                let base = base_path.and_then(|bp| prev_nodes.get(bp).map(|n| (bp, n)));
                if let Some((bp, b)) = base
                    && bp == &path
                    && node.untouched_since(b)
                {
                    continue;
                }
                let base = match (base, &prev_snapshot) {
                    (Some((bp, b)), Some(ps)) => b.manifest.clone().map(|m| (ps.join(bp), m)),
                    _ => None,
                };
                jobs.push(HashJob { path, node, base });
            }
        }
        Ok(Classified {
            upserts,
            removes,
            jobs,
            deferred,
            examined,
        })
    }

    /// Paths to examine and, with a journal, each one's last journal
    /// position.
    fn candidates(
        &self,
        snapshot: &Path,
        batch: &JournalBatch,
        full: bool,
    ) -> io::Result<(HashSet<PathBuf>, HashMap<PathBuf, u64>)> {
        let mut order: HashMap<PathBuf, u64> = HashMap::new();
        let mut candidates: HashSet<PathBuf> = HashSet::new();
        if full {
            walk(snapshot, Path::new(""), &mut candidates)?;
            if let Some(prev) = &self.prev {
                candidates.extend(prev.nodes.keys().cloned());
            }
        } else {
            for e in &batch.entries {
                for p in e.paths() {
                    order.insert(p.to_path_buf(), e.seq);
                    candidates.insert(p.to_path_buf());
                }
                let moves_subtree = e.is_dir
                    && matches!(
                        e.op,
                        JournalOp::Rename { .. } | JournalOp::Delete | JournalOp::Create
                    );
                if !moves_subtree {
                    continue;
                }
                for p in e.paths() {
                    let mut sub = HashSet::new();
                    walk(snapshot, p, &mut sub)?;
                    if let Some(prev) = &self.prev {
                        sub.extend(
                            prev.nodes
                                .range(p.to_path_buf()..)
                                .take_while(|(k, _)| k.starts_with(p))
                                .map(|(k, _)| k.clone()),
                        );
                    }
                    for s in sub {
                        order.entry(s.clone()).or_insert(e.seq);
                        candidates.insert(s);
                    }
                }
            }
        }
        candidates.remove(Path::new(""));
        Ok((candidates, order))
    }

    /// Deletes the retained snapshot. Call before dropping the capture
    /// when the snapshot is not needed for a restart.
    pub fn release(&mut self) -> io::Result<()> {
        if let Some(old) = self.prev.take() {
            btrfs::delete_subvolume(&old.snapshot)?;
        }
        Ok(())
    }
}

/// The captured state after applying `ops`. A full rescan starts from
/// nothing but keeps the previous node of every path that still exists
/// and did not change, so unchanged manifests are carried over.
fn next_state(
    prev: &BTreeMap<PathBuf, Node>,
    ops: &[ChangeOp],
    full: bool,
    snapshot: &Path,
) -> BTreeMap<PathBuf, Node> {
    let mut next = if full { BTreeMap::new() } else { prev.clone() };
    let mut gone = HashSet::new();
    for op in ops {
        match op {
            ChangeOp::Upsert { path, node } => {
                next.insert(path.clone(), node.clone());
            }
            ChangeOp::Remove { path, .. } => {
                next.remove(path);
                gone.insert(path.as_path());
            }
        }
    }
    if full {
        for (p, n) in prev {
            if !next.contains_key(p)
                && !gone.contains(p.as_path())
                && snapshot.join(p).symlink_metadata().is_ok()
            {
                next.insert(p.clone(), n.clone());
            }
        }
    }
    next
}

fn is_placeholder(abs: &Path) -> bool {
    matches!(sys::get_xattr(abs, RESIDENCY_XATTR), Ok(Some(_)))
}

/// Recursively lists `rel` (relative to `root`) and everything below it.
fn walk(root: &Path, rel: &Path, out: &mut HashSet<PathBuf>) -> io::Result<()> {
    let abs = root.join(rel);
    let meta = match fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    out.insert(rel.to_path_buf());
    if !meta.is_dir() {
        return Ok(());
    }
    let mut stack = vec![rel.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(root.join(&dir))? {
            let entry = entry?;
            let child = dir.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                stack.push(child.clone());
            }
            out.insert(child);
        }
    }
    Ok(())
}

/// Which paths are device-local, decided from the snapshot.
struct LocalOnly<'a> {
    snapshot: &'a Path,
    dirs: HashMap<PathBuf, bool>,
}

impl<'a> LocalOnly<'a> {
    fn new(snapshot: &'a Path) -> Self {
        Self {
            snapshot,
            dirs: HashMap::new(),
        }
    }

    fn marked(&self, rel: &Path) -> bool {
        let abs = self.snapshot.join(rel);
        if matches!(sys::get_xattr(&abs, LOCAL_ONLY_XATTR), Ok(Some(_))) {
            return true;
        }
        let tag = abs.join(CACHEDIR_TAG);
        let mut head = [0u8; 43];
        File::open(tag)
            .and_then(|mut f| io::Read::read_exact(&mut f, &mut head))
            .is_ok_and(|()| head == CACHEDIR_SIGNATURE)
    }

    fn contains(&mut self, rel: &Path) -> bool {
        if matches!(
            sys::get_xattr(&self.snapshot.join(rel), LOCAL_ONLY_XATTR),
            Ok(Some(_))
        ) {
            return true;
        }
        let mut anc = rel.parent();
        while let Some(a) = anc {
            if a.as_os_str().is_empty() {
                break;
            }
            let known = self.dirs.get(a).copied();
            let local = known.unwrap_or_else(|| self.marked(a));
            if known.is_none() {
                self.dirs.insert(a.to_path_buf(), local);
            }
            if local {
                return true;
            }
            anc = a.parent();
        }
        false
    }
}

fn hash_one(job: HashJob, snapshot: &Path) -> io::Result<((PathBuf, Node), ChunkStats)> {
    let HashJob {
        path,
        mut node,
        base,
    } = job;
    let file = File::open(snapshot.join(&path))?;
    let len = file.metadata()?.len();
    let (manifest, stats) = if let Some((base_file, base_manifest)) = base {
        let old = File::open(&base_file)?;
        let changed = extents::diff_files(&old, &file)?;
        if changed.is_empty() && len == base_manifest.total_len {
            let stats = ChunkStats {
                chunks_reused: base_manifest.chunks.len() as u64,
                ..ChunkStats::default()
            };
            (base_manifest, stats)
        } else {
            let map = sys::Mmap::map(&file, len)?;
            let (m, s) = Manifest::rebuild(Some(&base_manifest), map.as_slice(), &changed, None);
            (Arc::new(m), s)
        }
    } else {
        let map = sys::Mmap::map(&file, len)?;
        let (m, s) = Manifest::build(map.as_slice(), None);
        (Arc::new(m), s)
    };
    node.size = len;
    node.manifest = Some(manifest);
    Ok(((path, node), stats))
}

type HashResults = (Vec<(PathBuf, Node)>, ChunkStats);

fn run_jobs(jobs: Vec<HashJob>, threads: usize, snapshot: &Path) -> io::Result<HashResults> {
    if jobs.is_empty() {
        return Ok((Vec::new(), ChunkStats::default()));
    }
    let n = jobs.len();
    let jobs: Vec<Mutex<Option<HashJob>>> = jobs.into_iter().map(|j| Mutex::new(Some(j))).collect();
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(PathBuf, Node)>> = Mutex::new(Vec::with_capacity(n));
    let stats = Mutex::new(ChunkStats::default());
    let error: Mutex<Option<io::Error>> = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..threads.min(n) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(slot) = jobs.get(i) else { break };
                    let job = slot.lock().ok().and_then(|mut g| g.take());
                    let Some(job) = job else { continue };
                    match hash_one(job, snapshot) {
                        Ok((r, st)) => {
                            if let Ok(mut g) = results.lock() {
                                g.push(r);
                            }
                            if let Ok(mut g) = stats.lock() {
                                g.add(st);
                            }
                        }
                        // A file removed from the snapshot cannot happen (it
                        // is read-only); anything else fails the epoch.
                        Err(e) => {
                            if let Ok(mut g) = error.lock() {
                                g.get_or_insert(e);
                            }
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = error.into_inner().ok().flatten() {
        return Err(e);
    }
    Ok((
        results.into_inner().unwrap_or_default(),
        stats.into_inner().unwrap_or_default(),
    ))
}

/// Orders a changeset for application.
///
/// With a journal, operations follow the position of each path's last
/// journal entry (the writer's order). After a rescan there is no journal
/// order, and change time stands in for it. In both cases a directory is
/// created before anything inside it and removed after everything inside
/// it.
fn order_ops(
    upserts: Vec<(PathBuf, Node)>,
    removes: Vec<(PathBuf, bool)>,
    order: &HashMap<PathBuf, u64>,
    full: bool,
) -> Vec<ChangeOp> {
    #[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
    enum Key {
        Seq(u64),
        Time(i128),
    }
    let key_upsert = |p: &Path, n: &Node| {
        if full {
            Key::Time(n.ctime_ns)
        } else {
            Key::Seq(order.get(p).copied().unwrap_or(u64::MAX))
        }
    };
    let mut items: Vec<(Key, ChangeOp)> =
        Vec::with_capacity(upserts.len().saturating_add(removes.len()));
    for (path, node) in upserts {
        items.push((key_upsert(&path, &node), ChangeOp::Upsert { path, node }));
    }
    for (path, is_dir) in removes {
        let k = if full {
            Key::Time(i128::MAX)
        } else {
            Key::Seq(order.get(&path).copied().unwrap_or(u64::MAX))
        };
        items.push((k, ChangeOp::Remove { path, is_dir }));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path().cmp(b.1.path())));

    let mut upsert_at: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut remove_at: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for (i, (_, op)) in items.iter().enumerate() {
        match op {
            ChangeOp::Upsert { path, .. } => upsert_at.insert(path.clone(), i),
            ChangeOp::Remove { path, .. } => remove_at.insert(path.clone(), i),
        };
    }
    let mut slots: Vec<Option<ChangeOp>> = items.into_iter().map(|(_, op)| Some(op)).collect();
    let mut out = Vec::with_capacity(slots.len());
    let emit = |i: usize, slots: &mut Vec<Option<ChangeOp>>, out: &mut Vec<ChangeOp>| {
        if let Some(op) = slots.get_mut(i).and_then(Option::take) {
            out.push(op);
        }
    };
    for i in 0..slots.len() {
        let Some(op) = slots.get(i).and_then(Option::as_ref) else {
            continue;
        };
        match op {
            ChangeOp::Upsert { path, .. } => {
                // Ancestors first, outermost first.
                let path = path.clone();
                let mut chain: Vec<&Path> = path.ancestors().skip(1).collect();
                chain.reverse();
                for a in chain {
                    if let Some(&at) = upsert_at.get(a) {
                        emit(at, &mut slots, &mut out);
                    }
                }
            }
            ChangeOp::Remove { path, is_dir: true } => {
                // Descendants first, deepest first.
                let path = path.clone();
                let mut desc: Vec<(usize, usize)> = remove_at
                    .range(path.clone()..)
                    .skip(1)
                    .take_while(|(k, _)| k.starts_with(&path))
                    .map(|(k, v)| (k.components().count(), *v))
                    .collect();
                desc.sort_by_key(|d| std::cmp::Reverse(d.0));
                for (_, d) in desc {
                    emit(d, &mut slots, &mut out);
                }
            }
            ChangeOp::Remove { .. } => {}
        }
        emit(i, &mut slots, &mut out);
    }
    out
}

/// When to cut the next epoch.
///
/// Epochs are cut at quiescence (no event for `quiescence`), at the latest
/// `max_interval` after the first unpublished event, and later — up to
/// `grace_limit` — while a file created less than `fresh` ago is still open
/// for write. That keeps short-lived lock files out of most epochs with a
/// generic signal rather than a tool rule.
#[derive(Clone, Debug)]
pub struct EpochScheduler {
    pub quiescence: Duration,
    pub max_interval: Duration,
    pub fresh: Duration,
    pub grace_limit: Duration,
    first_dirty: Option<Instant>,
    last_event: Option<Instant>,
    /// Files created in this epoch that have not been closed after writing.
    open_fresh: HashMap<PathBuf, Instant>,
}

impl Default for EpochScheduler {
    fn default() -> Self {
        Self {
            quiescence: Duration::from_millis(500),
            max_interval: Duration::from_secs(2),
            fresh: Duration::from_secs(1),
            grace_limit: Duration::from_secs(5),
            first_dirty: None,
            last_event: None,
            open_fresh: HashMap::new(),
        }
    }
}

impl EpochScheduler {
    /// A scheduler with explicit tunables (the defaults are 500 ms, 2 s,
    /// 1 s and 5 s).
    pub fn new(
        quiescence: Duration,
        max_interval: Duration,
        fresh: Duration,
        grace_limit: Duration,
    ) -> Self {
        Self {
            quiescence,
            max_interval,
            fresh,
            grace_limit,
            ..Self::default()
        }
    }

    pub fn observe(&mut self, now: Instant, op: &JournalOp, path: Option<&Path>, is_dir: bool) {
        self.first_dirty.get_or_insert(now);
        self.last_event = Some(now);
        let Some(path) = path else { return };
        match op {
            JournalOp::Create if !is_dir => {
                self.open_fresh.insert(path.to_path_buf(), now);
            }
            JournalOp::CloseWrite | JournalOp::Delete | JournalOp::Rename { .. } => {
                self.open_fresh.remove(path);
            }
            _ => {}
        }
    }

    /// Whether an epoch should be cut at `now`.
    pub fn due(&self, now: Instant) -> bool {
        let (Some(first), Some(last)) = (self.first_dirty, self.last_event) else {
            return false;
        };
        let dirty_for = now.saturating_duration_since(first);
        let fresh_open = self
            .open_fresh
            .values()
            .any(|created| now.saturating_duration_since(*created) < self.fresh);
        if fresh_open {
            return dirty_for >= self.grace_limit;
        }
        now.saturating_duration_since(last) >= self.quiescence || dirty_for >= self.max_interval
    }

    /// Resets after an epoch was cut. Files still open stay tracked.
    pub fn cut(&mut self) {
        self.first_dirty = None;
        self.last_event = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: NodeKind, ctime: i128) -> Node {
        Node {
            kind,
            mode: 0o644,
            size: 0,
            mtime_ns: 0,
            manifest: None,
            ctime_ns: ctime,
            ino: 1,
        }
    }

    fn paths(ops: &[ChangeOp]) -> Vec<String> {
        ops.iter()
            .map(|o| match o {
                ChangeOp::Upsert { path, .. } => format!("+{}", path.display()),
                ChangeOp::Remove { path, .. } => format!("-{}", path.display()),
            })
            .collect()
    }

    #[test]
    fn journal_order_is_kept_and_parents_come_first() {
        let order: HashMap<PathBuf, u64> = [("a/obj", 1), ("a", 5), ("ref", 3)]
            .into_iter()
            .map(|(p, s)| (PathBuf::from(p), s))
            .collect();
        let ops = order_ops(
            vec![
                ("ref".into(), node(NodeKind::File, 0)),
                ("a/obj".into(), node(NodeKind::File, 0)),
                ("a".into(), node(NodeKind::Dir, 0)),
            ],
            vec![],
            &order,
            false,
        );
        assert_eq!(paths(&ops), ["+a", "+a/obj", "+ref"]);
    }

    #[test]
    fn directory_removal_follows_its_contents() {
        let order: HashMap<PathBuf, u64> = [("d", 1), ("d/x", 2), ("d/y/z", 3), ("d/y", 4)]
            .into_iter()
            .map(|(p, s)| (PathBuf::from(p), s))
            .collect();
        let ops = order_ops(
            vec![],
            vec![
                ("d".into(), true),
                ("d/x".into(), false),
                ("d/y".into(), true),
                ("d/y/z".into(), false),
            ],
            &order,
            false,
        );
        assert_eq!(paths(&ops), ["-d/y/z", "-d/x", "-d/y", "-d"]);
    }

    #[test]
    fn rescans_order_by_change_time() {
        let ops = order_ops(
            vec![
                ("late".into(), node(NodeKind::File, 20)),
                ("early".into(), node(NodeKind::File, 10)),
            ],
            vec![("gone".into(), false)],
            &HashMap::new(),
            true,
        );
        assert_eq!(paths(&ops), ["+early", "+late", "-gone"]);
    }

    #[test]
    fn scheduler_cuts_at_quiescence_or_max_interval() {
        let mut s = EpochScheduler::default();
        let t = Instant::now();
        assert!(!s.due(t));
        s.observe(t, &JournalOp::Modify, Some(Path::new("f")), false);
        assert!(!s.due(t + Duration::from_millis(100)));
        assert!(s.due(t + Duration::from_millis(600)));
        // A steady stream of events is still cut every 2 s.
        for i in 1..30 {
            s.observe(
                t + Duration::from_millis(i * 100),
                &JournalOp::Modify,
                None,
                false,
            );
        }
        assert!(s.due(t + Duration::from_millis(2_950)));
        s.cut();
        assert!(!s.due(t + Duration::from_secs(10)));
    }

    #[test]
    fn scheduler_waits_for_fresh_open_files_up_to_the_grace_limit() {
        let mut s = EpochScheduler::default();
        let t = Instant::now();
        s.observe(t, &JournalOp::Create, Some(Path::new("index.lock")), false);
        assert!(!s.due(t + Duration::from_millis(600)));
        s.observe(
            t + Duration::from_millis(700),
            &JournalOp::Create,
            Some(Path::new("b.lock")),
            false,
        );
        assert!(!s.due(t + Duration::from_millis(1_500)));
        assert!(s.due(t + Duration::from_secs(5)));
        s.observe(
            t + Duration::from_millis(800),
            &JournalOp::Rename { from: None },
            Some(Path::new("index.lock")),
            false,
        );
        s.observe(
            t + Duration::from_millis(800),
            &JournalOp::CloseWrite,
            Some(Path::new("b.lock")),
            false,
        );
        assert!(s.due(t + Duration::from_millis(1_400)));
    }
}
