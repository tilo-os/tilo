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
use crate::extents;
use crate::hydrate::RESIDENCY_XATTR;
use crate::journal::{ChangeJournal, JournalBatch, JournalOp};
use crate::sys;
use fs_content::{ChunkStats, Manifest};

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
                    && self.manifest.as_ref().map(|m| m.id())
                        == other.manifest.as_ref().map(|m| m.id())
            }
        }
    }

    fn state(&self) -> NodeState {
        NodeState {
            kind: self.kind.clone(),
            mode: self.mode,
            size: self.size,
            mtime_ns: self.mtime_ns,
            ctime_ns: self.ctime_ns,
            ino: self.ino,
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
    Upsert {
        path: PathBuf,
        node: Node,
    },
    Remove {
        path: PathBuf,
        is_dir: bool,
    },
    /// The inode at `from` now lives at `path`, and a directory brings
    /// everything below it. The record keeps its identity: history, access
    /// and references follow it. `changed` says the moved node's replicated
    /// state (content, size, mtime, mode) also changed; `node` is its state
    /// at `path`.
    ///
    /// `from` is in the coordinates of the tree as the preceding operations
    /// of the changeset left it; every other path is where the node is at
    /// the end of the changeset.
    Move {
        from: PathBuf,
        path: PathBuf,
        node: Node,
        changed: bool,
    },
}

impl ChangeOp {
    pub fn path(&self) -> &Path {
        match self {
            Self::Upsert { path, .. } | Self::Remove { path, .. } | Self::Move { path, .. } => path,
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
    /// Their content can only be captured once they are fully resident;
    /// they are examined again in the next epoch.
    pub deferred: Vec<PathBuf>,
    /// Placeholders now tracked without a published change: their content
    /// is fixed by the version they were created from, but a later local
    /// delete or rename of them must still be captured.
    pub tracked: Vec<PathBuf>,
    pub stats: CaptureStats,
}

/// The persistable part of one captured node. Manifests are not included:
/// after a restart a changed file is chunked from scratch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NodeState {
    pub kind: NodeKind,
    pub mode: u32,
    pub size: u64,
    pub mtime_ns: i128,
    pub ctime_ns: i128,
    pub ino: u64,
}

/// What a restart needs to continue from the last epoch: the retained
/// snapshot and the captured state of every path in it.
#[derive(Clone, Debug)]
pub struct CaptureState {
    /// Number the next epoch will get.
    pub next_epoch: u64,
    pub snapshot: PathBuf,
    pub nodes: Vec<(PathBuf, NodeState)>,
}

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    /// The room subvolume.
    pub live: PathBuf,
    /// Where epoch snapshots are created; same filesystem as `live`.
    pub epochs: PathBuf,
    pub hash_threads: usize,
    /// Build content manifests for changed files. A consumer that hashes
    /// the snapshot itself in another format turns this off; changes are
    /// then detected from size, mtime and mode, and the snapshot file is
    /// what it reads.
    pub hash_content: bool,
    /// Keep the previous epoch's snapshot until
    /// [`EpochCapture::release_retired`] is called, so a consumer can
    /// persist the new epoch before the one a restart would fall back to
    /// disappears.
    pub defer_release: bool,
}

impl CaptureConfig {
    pub fn new(live: impl Into<PathBuf>, epochs: impl Into<PathBuf>) -> Self {
        Self {
            live: live.into(),
            epochs: epochs.into(),
            hash_threads: std::thread::available_parallelism().map_or(2, |n| n.get().min(8)),
            hash_content: true,
            defer_release: false,
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
    /// Paths to examine in the next epoch even if the journal does not name
    /// them (deferred placeholders, paths a consumer asks for).
    dirty: HashSet<PathBuf>,
    /// Snapshots of earlier epochs kept alive by `defer_release`.
    retired: Vec<PathBuf>,
    /// Compare the whole room in the next epoch.
    rescan_next: bool,
}

struct Classified {
    upserts: Vec<(PathBuf, Node)>,
    removes: Vec<(PathBuf, bool)>,
    jobs: Vec<HashJob>,
    deferred: Vec<PathBuf>,
    tracked: Vec<(PathBuf, Node)>,
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
            dirty: HashSet::new(),
            retired: Vec::new(),
            rescan_next: false,
        })
    }

    /// Continues from a persisted epoch. The next capture compares the
    /// whole room against `state.snapshot`, which finds everything that
    /// changed while nothing was journaling (a crash, a stopped daemon) by
    /// its metadata; only files whose inode, size, mtime or ctime moved are
    /// read.
    pub fn resume(cfg: CaptureConfig, state: CaptureState) -> io::Result<Self> {
        fs::create_dir_all(&cfg.epochs)?;
        if !state.snapshot.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("retained snapshot {} is gone", state.snapshot.display()),
            ));
        }
        let nodes = state
            .nodes
            .into_iter()
            .map(|(path, n)| {
                (
                    path,
                    Node {
                        kind: n.kind,
                        mode: n.mode,
                        size: n.size,
                        mtime_ns: n.mtime_ns,
                        manifest: None,
                        ctime_ns: n.ctime_ns,
                        ino: n.ino,
                    },
                )
            })
            .collect();
        Ok(Self {
            cfg,
            prev: Some(EpochState {
                snapshot: state.snapshot,
                nodes,
            }),
            next_epoch: state.next_epoch.max(1),
            dirty: HashSet::new(),
            retired: Vec::new(),
            rescan_next: true,
        })
    }

    /// The state a restart continues from, while an epoch is retained.
    pub fn state(&self) -> Option<CaptureState> {
        let prev = self.prev.as_ref()?;
        Some(CaptureState {
            next_epoch: self.next_epoch,
            snapshot: prev.snapshot.clone(),
            nodes: prev
                .nodes
                .iter()
                .map(|(p, n)| (p.clone(), n.state()))
                .collect(),
        })
    }

    /// Number the next epoch will get.
    pub fn next_epoch(&self) -> u64 {
        self.next_epoch
    }

    /// The captured state of one path.
    pub fn node_state(&self, rel: &Path) -> Option<NodeState> {
        self.prev.as_ref()?.nodes.get(rel).map(Node::state)
    }

    /// Records the live state of `rel` as already captured, without
    /// publishing it: for changes the consumer made itself (applying a
    /// remote version), which the journal does not report. A later local
    /// change or removal of the path is then diffed against it.
    pub fn track(&mut self, rel: &Path) -> io::Result<()> {
        let Some(prev) = self.prev.as_mut() else {
            // The first epoch compares the whole room anyway.
            return Ok(());
        };
        let abs = self.cfg.live.join(rel);
        let meta = match fs::symlink_metadata(&abs) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                untrack_in(&mut prev.nodes, rel);
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let ft = meta.file_type();
        let kind = if ft.is_dir() {
            NodeKind::Dir
        } else if ft.is_symlink() {
            NodeKind::Symlink(fs::read_link(&abs)?)
        } else if ft.is_file() {
            NodeKind::File
        } else {
            return Ok(());
        };
        prev.nodes
            .insert(rel.to_path_buf(), Node::from_metadata(&meta, kind));
        Ok(())
    }

    /// Records that `from` and everything below it now live at `to`, for
    /// renames the consumer made itself (a remote move). A later local
    /// change is diffed against the moved nodes.
    pub fn track_move(&mut self, from: &Path, to: &Path) {
        if let Some(prev) = self.prev.as_mut() {
            move_nodes(&mut prev.nodes, None, from, to);
        }
    }

    /// The captured node of one path.
    pub fn node(&self, rel: &Path) -> Option<Node> {
        self.prev.as_ref()?.nodes.get(rel).cloned()
    }

    /// Paths tracked at or below `rel`.
    pub fn paths_below(&self, rel: &Path) -> Vec<PathBuf> {
        self.prev.as_ref().map_or_else(Vec::new, |p| {
            p.nodes
                .range(rel.to_path_buf()..)
                .take_while(|(k, _)| k.starts_with(rel))
                .map(|(k, _)| k.clone())
                .collect()
        })
    }

    /// Forgets `rel` and everything below it, for removals the consumer
    /// made itself.
    pub fn untrack(&mut self, rel: &Path) {
        if let Some(prev) = self.prev.as_mut() {
            untrack_in(&mut prev.nodes, rel);
        }
    }

    /// Keeps `rel` tracked but forgets that its current state was captured,
    /// for a change the consumer failed to publish: the next epoch captures
    /// the path again if it still exists, and its removal if it does not.
    pub fn invalidate(&mut self, rel: &Path) {
        if let Some(node) = self.prev.as_mut().and_then(|p| p.nodes.get_mut(rel)) {
            // No real inode matches these, so neither the unchanged-inode
            // shortcut nor the no-op filter can drop the next capture.
            node.ctime_ns = i128::MIN;
            node.mode = u32::MAX;
            node.manifest = None;
        }
        self.dirty.insert(rel.to_path_buf());
    }

    /// Examines `rel` in the next epoch even if the journal does not name it.
    pub fn mark_dirty(&mut self, rel: &Path) {
        self.dirty.insert(rel.to_path_buf());
    }

    /// Deletes the snapshots of earlier epochs kept by `defer_release`.
    pub fn release_retired(&mut self) -> io::Result<()> {
        while let Some(s) = self.retired.pop() {
            if let Err(e) = btrfs::delete_subvolume(&s) {
                self.retired.push(s);
                return Err(e);
            }
        }
        Ok(())
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
    ///
    /// `batch` must name every change that completed before the snapshot is
    /// taken, which holds only when nothing writes while this runs. With
    /// live writers use [`Self::capture_from`], which splits the journal at
    /// the snapshot.
    pub fn capture(&mut self, batch: &JournalBatch) -> io::Result<Changeset> {
        let t0 = Instant::now();
        let (epoch, snapshot) = self.take_snapshot()?;
        let t_snap = t0.elapsed();
        self.finish(t0, t_snap, epoch, snapshot, batch)
    }

    /// Cuts one epoch while writers keep running: barriers placed in the
    /// journal just before and just after the snapshot decide which
    /// entries the snapshot can contain. Entries from the window between
    /// the two are examined now and again in the next epoch, so no change
    /// is published before the journal entries of the changes it depends on.
    pub fn capture_from(&mut self, journal: &mut ChangeJournal) -> io::Result<Changeset> {
        let before = journal.barrier(&self.cfg.epochs)?;
        let t0 = Instant::now();
        let (epoch, snapshot) = self.take_snapshot()?;
        let t_snap = t0.elapsed();
        let after = journal.barrier(&self.cfg.epochs)?;
        let batch = journal.take_epoch(before, after)?;
        self.finish(t0, t_snap, epoch, snapshot, &batch)
    }

    fn take_snapshot(&mut self) -> io::Result<(u64, PathBuf)> {
        let epoch = self.next_epoch;
        let snapshot = self.cfg.epochs.join(format!("{epoch:016}"));
        btrfs::snapshot(&self.cfg.live, &snapshot, true)?;
        self.next_epoch = self.next_epoch.saturating_add(1);
        Ok((epoch, snapshot))
    }

    fn finish(
        &mut self,
        t0: Instant,
        t_snap: Duration,
        epoch: u64,
        snapshot: PathBuf,
        batch: &JournalBatch,
    ) -> io::Result<Changeset> {
        let full = self.prev.is_none() || batch.rescan.is_some() || self.rescan_next;
        self.rescan_next = false;
        let (candidates, mut order) = self.candidates(&snapshot, batch, full)?;

        let empty = BTreeMap::new();
        let prev_nodes = self.prev.as_ref().map_or(&empty, |p| &p.nodes);
        let Classified {
            mut upserts,
            removes,
            jobs,
            deferred,
            tracked,
            examined,
        } = self.classify(&snapshot, candidates, full)?;
        let t_scan = t0.elapsed();

        let (hashed, chunk_stats) = run_jobs(
            jobs,
            self.cfg.hash_threads.max(1),
            &snapshot,
            self.cfg.hash_content,
        )?;
        let files_hashed = hashed.len() as u64;
        upserts.extend(hashed);
        let t_hash = t0.elapsed();

        // Pair renames, drop no-op upserts, then order everything.
        let (mut upserts, removes, moves) = pair_moves(upserts, removes, prev_nodes, &mut order);
        upserts.retain(|(p, n)| prev_nodes.get(p).is_none_or(|old| !old.same_replicated(n)));
        let ops = order_ops(upserts, removes, moves, &order, full);
        let mut new_nodes = next_state(prev_nodes, &ops, full, &snapshot);
        let tracked_paths: Vec<PathBuf> = tracked.iter().map(|(p, _)| p.clone()).collect();
        new_nodes.extend(tracked);
        self.dirty.clear();
        self.dirty.extend(deferred.iter().cloned());
        if let Some(old) = self.prev.take() {
            if self.cfg.defer_release {
                self.retired.push(old.snapshot);
            } else {
                btrfs::delete_subvolume(&old.snapshot)?;
            }
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
            tracked: tracked_paths,
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
        let mut tracked = Vec::new();
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
                    !special && !is_staging(&path) && !local_only.contains(&path)
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
                    if !full && prev_nodes.contains_key(&path) {
                        deferred.push(path.clone());
                    }
                    tracked.push((path, node));
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
            tracked,
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
            for p in &self.dirty {
                order.insert(p.clone(), u64::MAX);
                candidates.insert(p.clone());
            }
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
            // A write through one hard link changes every path of the
            // inode, but the journal names only the one written through.
            if let Some(prev) = &self.prev {
                let mut by_ino: HashMap<u64, Vec<&PathBuf>> = HashMap::new();
                for (p, n) in &prev.nodes {
                    if n.kind == NodeKind::File {
                        by_ino.entry(n.ino).or_default().push(p);
                    }
                }
                let linked: Vec<(PathBuf, u64)> = candidates
                    .iter()
                    .filter_map(|p| {
                        let ino = prev.nodes.get(p)?.ino;
                        let seq = order.get(p).copied().unwrap_or(u64::MAX);
                        Some(
                            by_ino
                                .get(&ino)
                                .filter(|paths| paths.len() > 1)?
                                .iter()
                                .filter(|s| **s != p)
                                .map(|s| ((*s).clone(), seq))
                                .collect::<Vec<_>>(),
                        )
                    })
                    .flatten()
                    .collect();
                for (p, seq) in linked {
                    let at = order.entry(p.clone()).or_insert(seq);
                    *at = (*at).max(seq);
                    candidates.insert(p);
                }
            }
        }
        candidates.remove(Path::new(""));
        Ok((candidates, order))
    }

    /// Deletes the retained snapshot. Call before dropping the capture
    /// when the snapshot is not needed for a restart.
    pub fn release(&mut self) -> io::Result<()> {
        self.release_retired()?;
        if let Some(old) = self.prev.take() {
            btrfs::delete_subvolume(&old.snapshot)?;
        }
        Ok(())
    }
}

fn untrack_in(nodes: &mut BTreeMap<PathBuf, Node>, rel: &Path) {
    let below: Vec<PathBuf> = nodes
        .range(rel.to_path_buf()..)
        .take_while(|(k, _)| k.starts_with(rel))
        .map(|(k, _)| k.clone())
        .collect();
    for k in below {
        nodes.remove(&k);
    }
}

/// Whether two nodes are the same kind of thing, so one can be the other
/// renamed.
fn same_class(a: &NodeKind, b: &NodeKind) -> bool {
    matches!(
        (a, b),
        (NodeKind::File, NodeKind::File)
            | (NodeKind::Dir, NodeKind::Dir)
            | (NodeKind::Symlink(_), NodeKind::Symlink(_))
    )
}

/// `path` with its `from` prefix replaced by `to`, if it lies below `from`.
fn rebase(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    path.strip_prefix(from).ok().map(|rest| {
        if rest.as_os_str().is_empty() {
            to.to_path_buf()
        } else {
            to.join(rest)
        }
    })
}

type Paired = (Vec<(PathBuf, Node)>, Vec<(PathBuf, bool)>, Vec<ChangeOp>);

/// Turns a removal and an upsert of the same inode into a move.
///
/// A renamed file or directory keeps its inode, so the previous epoch's
/// node at the old path and the new node at the new path share it. Only a
/// path that is gone is a rename source: an inode still present at its old
/// path as well is a hard link, captured as a separate file. Moves implied
/// by a directory's move (its descendants, at the same relative path) are
/// not repeated; a descendant that also changed stays an upsert at its new
/// path. A removal below a moved directory is rewritten to where the
/// directory went, since that is where peers will have it.
fn pair_moves(
    upserts: Vec<(PathBuf, Node)>,
    removes: Vec<(PathBuf, bool)>,
    prev: &BTreeMap<PathBuf, Node>,
    order: &mut HashMap<PathBuf, u64>,
) -> Paired {
    let mut removed_by_ino: HashMap<u64, Option<&PathBuf>> = HashMap::new();
    for (path, _) in &removes {
        if let Some(old) = prev.get(path) {
            // Two removed paths with one inode (hard links both gone):
            // ambiguous, so neither becomes a move.
            removed_by_ino
                .entry(old.ino)
                .and_modify(|p| *p = None)
                .or_insert(Some(path));
        }
    }
    let mut candidates: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (path, node) in &upserts {
        if prev.get(path).is_some_and(|old| old.ino == node.ino) {
            continue;
        }
        let Some(Some(from)) = removed_by_ino.get(&node.ino) else {
            continue;
        };
        let Some(old) = prev.get(*from) else { continue };
        if same_class(&old.kind, &node.kind) {
            candidates.push(((*from).clone(), path.clone()));
        }
    }
    // A move is implied when a moved ancestor explains it.
    let implied = |from: &Path, to: &Path| {
        candidates.iter().any(|(pf, pt)| {
            pf.as_path() != from
                && from.starts_with(pf)
                && rebase(from, pf, pt).as_deref() == Some(to)
        })
    };
    let mut direct: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut consumed_from: HashSet<PathBuf> = HashSet::new();
    let mut consumed_to: HashMap<PathBuf, bool> = HashMap::new();
    for (from, to) in &candidates {
        consumed_from.insert(from.clone());
        if implied(from, to) {
            consumed_to.insert(to.clone(), false);
        } else {
            consumed_to.insert(to.clone(), true);
            direct.push((from.clone(), to.clone()));
        }
    }
    let mut kept = Vec::with_capacity(upserts.len());
    let mut moves = Vec::new();
    for (path, node) in upserts {
        match consumed_to.get(&path) {
            None => kept.push((path, node)),
            Some(is_direct) => {
                let Some((from, _)) = candidates.iter().find(|(_, t)| *t == path) else {
                    kept.push((path, node));
                    continue;
                };
                let changed = prev.get(from).is_none_or(|old| !old.same_replicated(&node));
                if *is_direct {
                    moves.push(ChangeOp::Move {
                        from: from.clone(),
                        path,
                        node,
                        changed,
                    });
                } else if changed {
                    kept.push((path, node));
                }
            }
        }
    }
    // Longest source first, so the innermost move rewrites a removal.
    direct.sort_by_key(|(from, _)| std::cmp::Reverse(from.components().count()));
    let removes = removes
        .into_iter()
        .filter(|(path, _)| !consumed_from.contains(path))
        .map(|(path, is_dir)| {
            let target = direct
                .iter()
                .find_map(|(from, to)| rebase(&path, from, to).filter(|_| path != *from));
            match target {
                Some(new) => {
                    // It keeps its journal position.
                    if let Some(seq) = order.get(&path).copied() {
                        order.insert(new.clone(), seq);
                    }
                    (new, is_dir)
                }
                None => (path, is_dir),
            }
        })
        .collect();
    (kept, removes, moves)
}

/// Applies a move to a node table: `from` and everything below it now live
/// under `to`. Entries are taken from `source` (the table being built, or
/// the previous epoch's after a full comparison, which starts empty).
fn move_nodes(
    next: &mut BTreeMap<PathBuf, Node>,
    source: Option<&BTreeMap<PathBuf, Node>>,
    from: &Path,
    to: &Path,
) {
    let taken: Vec<(PathBuf, Node)> = {
        let table = source.unwrap_or(next);
        table
            .range(from.to_path_buf()..)
            .take_while(|(k, _)| k.starts_with(from))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    for (k, _) in &taken {
        next.remove(k);
    }
    for (k, v) in taken {
        if let Some(moved) = rebase(&k, from, to) {
            next.insert(moved, v);
        }
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
    // After a full comparison `next` starts empty; a move's unchanged
    // descendants come from the previous table, in its coordinates.
    let mut moved_prev: Vec<(PathBuf, PathBuf)> = Vec::new();
    for op in ops {
        match op {
            ChangeOp::Upsert { path, node } => {
                next.insert(path.clone(), node.clone());
            }
            ChangeOp::Remove { path, .. } => {
                untrack_in(&mut next, path);
                gone.insert(path.as_path());
            }
            ChangeOp::Move {
                from, path, node, ..
            } => {
                if full {
                    // Back to the previous table's coordinates.
                    let original = moved_prev
                        .iter()
                        .filter(|(_, t)| from.starts_with(t))
                        .max_by_key(|(_, t)| t.components().count())
                        .and_then(|(f, t)| rebase(from, t, f))
                        .unwrap_or_else(|| from.clone());
                    let mut staged = BTreeMap::new();
                    move_nodes(&mut staged, Some(prev), &original, path);
                    untrack_in(&mut next, from);
                    for (k, v) in staged {
                        next.entry(k).or_insert(v);
                    }
                    moved_prev.push((original, path.clone()));
                    gone.insert(from.as_path());
                } else {
                    move_nodes(&mut next, None, from, path);
                }
                next.insert(path.clone(), node.clone());
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

/// Staging files are the daemon's own half-written versions, renamed into
/// place when complete (`hydrate::STAGING_PREFIX`); they are never content.
fn is_staging(rel: &Path) -> bool {
    rel.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(crate::hydrate::STAGING_PREFIX))
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

fn hash_one(
    job: HashJob,
    snapshot: &Path,
    hash_content: bool,
) -> io::Result<((PathBuf, Node), ChunkStats)> {
    let HashJob {
        path,
        mut node,
        base,
    } = job;
    let file = File::open(snapshot.join(&path))?;
    let len = file.metadata()?.len();
    if !hash_content {
        node.size = len;
        return Ok(((path, node), ChunkStats::default()));
    }
    let (manifest, stats) = if let Some((base_file, base_manifest)) = base {
        let old = File::open(&base_file)?;
        let changed = extents::diff_files(&old, &file)?;
        if changed.is_empty() && len == base_manifest.total_len() {
            let stats = ChunkStats {
                chunks_reused: base_manifest.chunks().len() as u64,
                ..ChunkStats::default()
            };
            (base_manifest, stats)
        } else {
            let map = sys::Mmap::map(&file, len)?;
            let (m, s) = fs_content::rebuild(Some(&base_manifest), map.as_slice(), &changed, None);
            (Arc::new(m), s)
        }
    } else {
        let map = sys::Mmap::map(&file, len)?;
        let (m, s) = fs_content::build(map.as_slice(), None);
        (Arc::new(m), s)
    };
    node.size = len;
    node.manifest = Some(manifest);
    Ok(((path, node), stats))
}

type HashResults = (Vec<(PathBuf, Node)>, ChunkStats);

fn run_jobs(
    jobs: Vec<HashJob>,
    threads: usize,
    snapshot: &Path,
    hash_content: bool,
) -> io::Result<HashResults> {
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
                    match hash_one(job, snapshot, hash_content) {
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
/// created (or moved into place) before anything inside it and removed
/// after everything inside it. A move's source is then rewritten to where
/// the moves before it left it.
fn order_ops(
    upserts: Vec<(PathBuf, Node)>,
    removes: Vec<(PathBuf, bool)>,
    moves: Vec<ChangeOp>,
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
    let mut items: Vec<(Key, ChangeOp)> = Vec::with_capacity(
        upserts
            .len()
            .saturating_add(removes.len())
            .saturating_add(moves.len()),
    );
    for (path, node) in upserts {
        items.push((key_upsert(&path, &node), ChangeOp::Upsert { path, node }));
    }
    for op in moves {
        if let ChangeOp::Move { path, node, .. } = &op {
            items.push((key_upsert(path, node), op));
        }
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

    // What must be in place before a path can be touched: its ancestors,
    // created or moved there.
    let mut provides: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    let mut remove_at: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for (i, (_, op)) in items.iter().enumerate() {
        match op {
            ChangeOp::Upsert { path, .. } | ChangeOp::Move { path, .. } => {
                provides.entry(path.clone()).or_default().push(i);
            }
            ChangeOp::Remove { path, .. } => {
                remove_at.insert(path.clone(), i);
            }
        }
    }
    let mut slots: Vec<Option<ChangeOp>> = items.into_iter().map(|(_, op)| Some(op)).collect();
    let mut out = Vec::with_capacity(slots.len());
    let mut visiting = vec![false; slots.len()];
    for i in 0..slots.len() {
        place(
            i,
            &mut slots,
            &mut visiting,
            &provides,
            &remove_at,
            &mut out,
        );
    }
    rewrite_move_sources(&mut out);
    out
}

/// Emits operation `i` after everything it depends on.
fn place(
    i: usize,
    slots: &mut Vec<Option<ChangeOp>>,
    visiting: &mut Vec<bool>,
    provides: &BTreeMap<PathBuf, Vec<usize>>,
    remove_at: &BTreeMap<PathBuf, usize>,
    out: &mut Vec<ChangeOp>,
) {
    let Some(op) = slots.get(i).and_then(Option::as_ref) else {
        return;
    };
    if visiting.get(i).copied().unwrap_or(true) {
        return;
    }
    if let Some(v) = visiting.get_mut(i) {
        *v = true;
    }
    let path = op.path().to_path_buf();
    let mut before: Vec<usize> = Vec::new();
    // Ancestors first, outermost first.
    let mut chain: Vec<&Path> = path.ancestors().skip(1).collect();
    chain.reverse();
    for a in chain {
        if let Some(at) = provides.get(a) {
            before.extend(at.iter().copied());
        }
    }
    if let ChangeOp::Remove { is_dir: true, .. } = op {
        // Descendants first, deepest first.
        let mut desc: Vec<(usize, usize)> = remove_at
            .range(path.clone()..)
            .skip(1)
            .take_while(|(k, _)| k.starts_with(&path))
            .map(|(k, v)| (k.components().count(), *v))
            .collect();
        desc.sort_by_key(|d| std::cmp::Reverse(d.0));
        before.extend(desc.into_iter().map(|(_, d)| d));
    }
    for b in before {
        if b != i {
            place(b, slots, visiting, provides, remove_at, out);
        }
    }
    if let Some(op) = slots.get_mut(i).and_then(Option::take) {
        out.push(op);
    }
}

/// Moves are paired in the previous epoch's coordinates; each source is
/// rewritten through the moves emitted before it.
fn rewrite_move_sources(ops: &mut [ChangeOp]) {
    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    for op in ops.iter_mut() {
        if let ChangeOp::Move { from, path, .. } = op {
            let original = from.clone();
            let innermost = done
                .iter()
                .filter(|(f, _)| original.starts_with(f))
                .max_by_key(|(f, _)| f.components().count());
            if let Some(moved) = innermost.and_then(|(f, t)| rebase(&original, f, t)) {
                *from = moved;
            }
            done.push((original, path.clone()));
        }
    }
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
                ChangeOp::Move { from, path, .. } => {
                    format!("{}>{}", from.display(), path.display())
                }
            })
            .collect()
    }

    #[test]
    fn journal_batches_order_parents_removals_and_rescans() {
        // Phase `journal_order_is_kept_and_parents_come_first`.
        {
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
                vec![],
                &order,
                false,
            );
            assert_eq!(paths(&ops), ["+a", "+a/obj", "+ref"]);
        }
        // Phase `directory_removal_follows_its_contents`.
        {
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
                vec![],
                &order,
                false,
            );
            assert_eq!(paths(&ops), ["-d/y/z", "-d/x", "-d/y", "-d"]);
        }
        // Phase `rescans_order_by_change_time`.
        {
            let ops = order_ops(
                vec![
                    ("late".into(), node(NodeKind::File, 20)),
                    ("early".into(), node(NodeKind::File, 10)),
                ],
                vec![("gone".into(), false)],
                vec![],
                &HashMap::new(),
                true,
            );
            assert_eq!(paths(&ops), ["+early", "+late", "-gone"]);
        }
    }

    fn prev(entries: &[(&str, NodeKind, u64)]) -> BTreeMap<PathBuf, Node> {
        entries
            .iter()
            .map(|(p, k, ino)| {
                (
                    PathBuf::from(p),
                    Node {
                        ino: *ino,
                        ..node(k.clone(), 0)
                    },
                )
            })
            .collect()
    }

    fn at(path: &str, kind: NodeKind, ino: u64) -> (PathBuf, Node) {
        (
            path.into(),
            Node {
                ino,
                ..node(kind, 0)
            },
        )
    }

    #[test]
    fn moves_and_hard_links_are_told_apart_by_inode_and_path() {
        // Phase `a_directory_move_carries_its_subtree_and_rewrites_removals_inside_it`.
        {
            let before = prev(&[
                ("a", NodeKind::Dir, 1),
                ("a/f", NodeKind::File, 2),
                ("a/gone", NodeKind::File, 3),
                ("a/out", NodeKind::File, 4),
            ]);
            let mut order: HashMap<PathBuf, u64> = [
                ("z", 2),
                ("a", 2),
                ("a/gone", 1),
                ("elsewhere", 3),
                ("a/out", 3),
            ]
            .into_iter()
            .map(|(p, s)| (PathBuf::from(p), s))
            .collect();
            let (ups, rms, mv) = pair_moves(
                vec![
                    at("z", NodeKind::Dir, 1),
                    at("z/f", NodeKind::File, 2),
                    at("elsewhere", NodeKind::File, 4),
                ],
                vec![
                    ("a".into(), true),
                    ("a/f".into(), false),
                    ("a/gone".into(), false),
                    ("a/out".into(), false),
                ],
                &before,
                &mut order,
            );
            assert!(ups.is_empty(), "the unchanged descendant is implied");
            assert_eq!(rms, [(PathBuf::from("z/gone"), false)]);
            assert_eq!(order.get(Path::new("z/gone")), Some(&1));
            let ops = order_ops(ups, rms, mv, &order, false);
            assert_eq!(paths(&ops), ["a>z", "-z/gone", "z/out>elsewhere"]);
            let next = next_state(&before, &ops, false, Path::new("/nonexistent"));
            let keys: Vec<&str> = next.keys().filter_map(|p| p.to_str()).collect();
            assert_eq!(keys, ["elsewhere", "z", "z/f"]);
        }
        // Phase `an_inode_still_at_its_old_path_is_a_hard_link_not_a_move`.
        {
            let before = prev(&[("a", NodeKind::File, 7)]);
            let (ups, rms, mv) = pair_moves(
                vec![at("a", NodeKind::File, 7), at("b", NodeKind::File, 7)],
                vec![],
                &before,
                &mut HashMap::new(),
            );
            assert_eq!(ups.len(), 2);
            assert!(rms.is_empty() && mv.is_empty());
            // A file replaced by a directory of the same inode number is not a
            // move either.
            let (ups, _, mv) = pair_moves(
                vec![at("d", NodeKind::Dir, 7)],
                vec![("a".into(), false)],
                &before,
                &mut HashMap::new(),
            );
            assert_eq!(ups.len(), 1);
            assert!(mv.is_empty());
        }
    }

    #[test]
    fn the_epoch_scheduler_cuts_at_quiescence_interval_or_grace_limit() {
        // Phase `scheduler_cuts_at_quiescence_or_max_interval`.
        {
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
        // Phase `scheduler_waits_for_fresh_open_files_up_to_the_grace_limit`.
        {
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
}
