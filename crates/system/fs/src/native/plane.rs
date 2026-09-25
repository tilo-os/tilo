//! The running Tier 1 data plane of one workspace.
//!
//! Startup order is what keeps the tree safe to use:
//!
//! 1. the room's subvolume on the store volume exists;
//! 2. the pre-content group starts, and every placeholder left by an
//!    earlier run is re-adopted from its residency xattr — a placeholder
//!    read without the daemon holding its mark returns zeros, so none may
//!    be reachable before this;
//! 3. the change journal starts, ignoring this process;
//! 4. the first epoch is cut: from the persisted epoch when there is one
//!    (a comparison by inode metadata finds what changed while nothing was
//!    journaling), else from scratch;
//! 5. only then is the room bind-mounted at the workspace path.
//!
//! Stopping reverses it: the bind goes first, pending changes are cut into
//! a last epoch, held leases are released, and the pre-content group
//! closes last.
//!
//! One thread owns the journal, the capture and the projection, so a local
//! change and a remote version of the same path are never applied
//! concurrently.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use fs_native::capture::{
    CaptureConfig, ChangeOp, Changeset, EpochCapture, EpochScheduler, NodeKind,
};
use fs_native::hydrate::{Hydrator as Placeholders, HydratorConfig};
use fs_native::journal::{ChangeJournal, JournalConfig, JournalOp};
use fs_native::store::StoreLayout;

use super::fetch::{ContentFetcher, DEFAULT_PARALLEL_CHUNKS};
use super::lease::{LeaseKeeper, LeaseTiming};
use super::project::{self, Projector};
use super::state::{LocalStat, Materialized, NativeState, NodeUpdate, StoredLease};
use crate::fs::QuickFS;
use crate::hydration::Hydrator;
use crate::sync::SyncHandle;

/// How often the record plane is re-projected without a change signal.
const PROJECT_FALLBACK: Duration = Duration::from_secs(3);
const LEASE_TICK: Duration = Duration::from_secs(1);
/// How soon a projection waiting for a version is tried again.
const WAITING_RETRY: Duration = Duration::from_millis(100);
/// Delay before a failed publication is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(2);

/// Everything the plane needs to know about its workspace.
#[derive(Clone, Debug)]
pub struct NativeConfig {
    /// Root of the dedicated store volume.
    pub store: PathBuf,
    /// Room (workspace) id: the name of its subvolume under `live/`.
    pub room: String,
    /// Where the room is bind-mounted (`/var/workspace`).
    pub mount_point: PathBuf,
    /// Directory holding `meta.redb`.
    pub state_dir: PathBuf,
    /// Owner of materialised files; `None` takes the room directory's.
    pub owner: Option<(u32, u32)>,
    /// This device's key, as leases name their holder.
    pub device: [u8; 32],
    /// Remote files up to this size are fetched before they appear.
    pub eager_bytes: u64,
    pub scheduler: EpochScheduler,
    pub leases: LeaseTiming,
}

impl NativeConfig {
    pub fn new(
        store: PathBuf,
        room: String,
        mount_point: PathBuf,
        state_dir: PathBuf,
        device: [u8; 32],
    ) -> Self {
        Self {
            store,
            room,
            mount_point,
            state_dir,
            owner: None,
            device,
            eager_bytes: 1024 * 1024,
            scheduler: EpochScheduler::default(),
            leases: LeaseTiming::default(),
        }
    }
}

/// Counters for logs and tests.
#[derive(Clone, Debug, Default)]
pub struct PlaneStatus {
    pub epochs: u64,
    pub ops_published: u64,
    pub bytes_published: u64,
    pub projections: u64,
    pub files_materialized: u64,
    pub placeholders_created: u64,
    pub placeholders_adopted: usize,
    pub last_error: Option<String>,
}

pub struct NativePlane {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
    status: Arc<Mutex<PlaneStatus>>,
    mount_point: PathBuf,
}

fn mountinfo_has(mount_point: &Path) -> bool {
    let Ok(canonical) = std::fs::canonicalize(mount_point) else {
        return false;
    };
    let Ok(info) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    info.lines().any(|line| {
        line.split(' ')
            .nth(4)
            .is_some_and(|mp| Path::new(&unescape(mp)) == canonical)
    })
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b == b'\\'
            && let Some(code) = s.get(i.saturating_add(1)..i.saturating_add(4))
            && let Ok(v) = u8::from_str_radix(code, 8)
        {
            out.push(char::from(v));
            i = i.saturating_add(4);
        } else {
            out.push(char::from(b));
            i = i.saturating_add(1);
        }
    }
    out
}

fn c_path(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes()).context("path contains a NUL byte")
}

/// Detaches whatever is mounted at `mount_point` (a bind a crashed run left,
/// or a stale FUSE mount).
fn detach(mount_point: &Path) -> Result<()> {
    let target = c_path(mount_point)?;
    for _ in 0..8 {
        if !mountinfo_has(mount_point) {
            return Ok(());
        }
        // SAFETY: `target` is a valid NUL-terminated path.
        let rc = unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("detaching {}", mount_point.display()));
        }
    }
    Ok(())
}

fn bind(source: &Path, target: &Path) -> Result<()> {
    let src = c_path(source)?;
    let dst = c_path(target)?;
    // SAFETY: both paths are valid NUL-terminated strings; a bind mount
    // takes no filesystem type or data.
    let rc = unsafe {
        libc::mount(
            src.as_ptr(),
            dst.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!("bind-mounting {} at {}", source.display(), target.display())
        });
    }
    Ok(())
}

struct Engine {
    cfg: NativeConfig,
    fs: Arc<QuickFS>,
    live: PathBuf,
    state: NativeState,
    journal: ChangeJournal,
    capture: EpochCapture,
    scheduler: EpochScheduler,
    placeholders: Arc<Placeholders>,
    projector: Projector,
    leases: LeaseKeeper,
    materialized: HashMap<PathBuf, Materialized>,
    /// Paths whose captured node changed outside an epoch (tracked after a
    /// projection); persisted with the next epoch.
    node_dirty: HashSet<PathBuf>,
    /// Journal positions already fed to the scheduler.
    observed: u64,
    /// Paths written in place since the last epoch, and paths created or
    /// renamed into place (which are not in-place writes).
    modified: HashSet<PathBuf>,
    replaced: HashSet<PathBuf>,
    status: Arc<Mutex<PlaneStatus>>,
    symlink_warned: bool,
    /// Removals the record plane refused, retried every epoch.
    retry_removals: HashSet<PathBuf>,
    /// When to cut an epoch for failed publications even without new changes.
    retry_at: Option<Instant>,
    /// The last projection stopped at a version not yet verified here.
    waiting: bool,
    waiting_for: Option<PathBuf>,
    /// Log every journal entry and published operation.
    trace: bool,
}

impl NativePlane {
    /// Sets the plane up and exposes the tree. Returns once the room is
    /// bind-mounted at the workspace path.
    pub fn start(
        cfg: NativeConfig,
        fs: Arc<QuickFS>,
        content: Arc<Hydrator>,
        sync: Option<Arc<SyncHandle>>,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self> {
        let status = Arc::new(Mutex::new(PlaneStatus::default()));
        let mut engine = Engine::open(cfg.clone(), fs, content, runtime, Arc::clone(&status))?;
        let t = Instant::now();
        engine.initial_epoch()?;
        engine.project();
        eprintln!(
            "native data plane: initial epoch and projection in {} ms",
            t.elapsed().as_millis()
        );
        std::fs::create_dir_all(&cfg.mount_point)?;
        detach(&cfg.mount_point)?;
        bind(&engine.live, &cfg.mount_point)?;
        println!(
            "Serving workspace natively from {} at {}",
            engine.live.display(),
            cfg.mount_point.display()
        );
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("native-plane".into())
                .spawn(move || engine.run(&stop, sync))
                .context("starting the native data plane thread")?
        };
        Ok(Self {
            stop,
            thread: Some(thread),
            status,
            mount_point: cfg.mount_point,
        })
    }

    pub fn status(&self) -> PlaneStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Whether the plane thread ended on its own (an error).
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Unmounts, publishes what is pending and stops serving placeholders.
    pub fn stop(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        match self.thread.take() {
            Some(t) => t
                .join()
                .map_err(|_| anyhow!("native data plane thread panicked"))?,
            None => Ok(()),
        }
    }
}

impl Drop for NativePlane {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // The bind never outlives the plane, even on an error path.
        let _ = detach(&self.mount_point);
    }
}

impl Engine {
    fn open(
        cfg: NativeConfig,
        fs: Arc<QuickFS>,
        content: Arc<Hydrator>,
        runtime: tokio::runtime::Handle,
        status: Arc<Mutex<PlaneStatus>>,
    ) -> Result<Self> {
        let layout = StoreLayout::new(&cfg.store);
        let (live, owner) = prepare_room(&layout, &cfg)?;
        let epochs = layout.epochs().join(&cfg.room);
        std::fs::create_dir_all(&epochs)?;
        std::fs::create_dir_all(&cfg.state_dir)?;
        let state = NativeState::open(&cfg.state_dir.join("meta.redb"))?;
        let placeholders = start_placeholders(&live, &content, &runtime, &status)?;

        let persisted = state.capture_state()?;
        let mut journal_cfg = JournalConfig::new(&live);
        journal_cfg.ignore_pids = vec![i32::try_from(std::process::id()).unwrap_or(i32::MAX)];
        journal_cfg.first_seq = persisted
            .as_ref()
            .map_or(1, |(_, through)| through.saturating_add(1));
        let journal = ChangeJournal::open(&cfg.store, journal_cfg)
            .context("opening the change journal (needs CAP_SYS_ADMIN and CAP_DAC_READ_SEARCH)")?;
        let capture = open_capture(&live, &epochs, &state, persisted.map(|(c, _)| c))?;

        let materialized = state.materialized()?;
        let lease_class: HashSet<PathBuf> = state.leases()?.into_keys().collect();
        let write_denied = Arc::new(Mutex::new(HashSet::new()));
        let leases = LeaseKeeper::new(
            Arc::clone(&fs),
            Arc::clone(&placeholders),
            live.clone(),
            cfg.device,
            cfg.leases,
            lease_class,
            Arc::clone(&write_denied),
        );
        let mut projector = Projector::new(
            live.clone(),
            Arc::clone(&placeholders),
            content,
            runtime,
            owner,
            write_denied,
        );
        projector.eager_bytes = cfg.eager_bytes;
        let scheduler = cfg.scheduler.clone();
        Ok(Self {
            cfg,
            fs,
            live,
            state,
            journal,
            capture,
            scheduler,
            placeholders,
            projector,
            leases,
            materialized,
            node_dirty: HashSet::new(),
            observed: 0,
            modified: HashSet::new(),
            replaced: HashSet::new(),
            status,
            symlink_warned: false,
            retry_removals: HashSet::new(),
            retry_at: None,
            waiting: false,
            waiting_for: None,
            trace: std::env::var_os("TILO_FS_NATIVE_TRACE").is_some(),
        })
    }

    /// The first epoch of this run. Without a persisted epoch the capture
    /// cannot see deletions made while stopped; a path the daemon had
    /// materialised or published and that is gone now was deleted locally.
    fn initial_epoch(&mut self) -> Result<()> {
        let resumed = self.capture.last_snapshot().is_some();
        self.cut_epoch()?;
        if !resumed {
            let mut gone: Vec<PathBuf> = self
                .materialized
                .iter()
                .filter(|(p, m)| !m.dir && LocalStat::of(&self.live.join(p)).is_none())
                .map(|(p, _)| p.clone())
                .collect();
            gone.sort();
            let mut updates = Vec::new();
            for rel in gone {
                if let Err(errno) = self.fs.apply_local_remove(&rel) {
                    eprintln!("publishing the removal of {}: errno {errno}", rel.display());
                    continue;
                }
                self.materialized.remove(&rel);
                updates.push((rel, None));
            }
            self.state.set_materialized(&updates)?;
        }
        Ok(())
    }

    fn run(mut self, stop: &AtomicBool, sync: Option<Arc<SyncHandle>>) -> Result<()> {
        let mut generation = sync.map(|s| s.subscribe_record_index_generation());
        let mut last_project = Instant::now();
        let mut last_lease = Instant::now()
            .checked_sub(LEASE_TICK)
            .unwrap_or_else(Instant::now);
        let result = loop {
            if stop.load(Ordering::Relaxed) {
                break Ok(());
            }
            if let Err(error) = self.journal.pump(Some(Duration::from_millis(50))) {
                break Err(anyhow!(error).context("reading the change journal"));
            }
            self.observe_journal();
            let now = Instant::now();
            let retry = self.retry_at.is_some_and(|at| now >= at);
            if self.scheduler.due(now) || retry {
                self.retry_at = None;
                if let Err(error) = self.cut_epoch() {
                    self.note_error(&error);
                }
                self.scheduler.cut();
            }
            let remote = generation
                .as_mut()
                .is_some_and(|g| g.has_changed().unwrap_or(false));
            let again = self.waiting && last_project.elapsed() >= WAITING_RETRY;
            if remote || again || last_project.elapsed() >= PROJECT_FALLBACK {
                if let Some(g) = generation.as_mut() {
                    g.borrow_and_update();
                }
                self.project();
                last_project = Instant::now();
            }
            if last_lease.elapsed() >= LEASE_TICK {
                self.leases.tick(QuickFS::record_clock_ms());
                last_lease = Instant::now();
            }
        };
        self.shutdown();
        result
    }

    fn note_error(&self, error: &anyhow::Error) {
        eprintln!("native data plane: {error:#}");
        if let Ok(mut s) = self.status.lock() {
            s.last_error = Some(format!("{error:#}"));
        }
    }

    fn shutdown(mut self) {
        if let Err(error) = detach(&self.cfg.mount_point) {
            eprintln!("{error:#}");
        }
        let _ = self
            .journal
            .pump_until_idle(Duration::from_millis(100), Duration::from_secs(2));
        self.observe_journal();
        if (!self.journal.pending().is_empty() || !self.node_dirty.is_empty())
            && let Err(error) = self.cut_epoch()
        {
            self.note_error(&error);
        }
        self.leases.release_all(QuickFS::record_clock_ms());
        let Self {
            placeholders,
            projector,
            leases,
            ..
        } = self;
        drop(projector);
        drop(leases);
        // Last: once the group closes, placeholders read as holes.
        if let Ok(p) = Arc::try_unwrap(placeholders) {
            p.stop();
        }
    }

    fn observe_journal(&mut self) {
        let now = Instant::now();
        for e in self.journal.pending() {
            if e.seq <= self.observed {
                continue;
            }
            self.observed = e.seq;
            if self.trace {
                eprintln!(
                    "  journal {} {:?} {:?} pid {}",
                    e.seq,
                    e.op,
                    e.path.as_deref().map(Path::display),
                    e.pid
                );
            }
            self.scheduler
                .observe(now, &e.op, e.path.as_deref(), e.is_dir);
            let Some(path) = e.path.as_ref() else {
                continue;
            };
            match e.op {
                JournalOp::Modify => {
                    self.modified.insert(path.clone());
                    self.leases.note_write(path);
                }
                JournalOp::Create | JournalOp::Rename { .. } => {
                    self.replaced.insert(path.clone());
                }
                JournalOp::CloseWrite => self.leases.note_close(path),
                _ => {}
            }
        }
    }

    /// Cuts an epoch and publishes its changeset in journal order.
    fn cut_epoch(&mut self) -> Result<()> {
        let t = Instant::now();
        let previous_snapshot = self.capture.last_snapshot().map(Path::to_path_buf);
        let in_place: Vec<(PathBuf, u64)> = self
            .modified
            .iter()
            .filter(|p| !self.replaced.contains(*p))
            .filter_map(|p| {
                let old = self.capture.node_state(p)?;
                (old.kind == NodeKind::File && old.size > 0).then(|| (p.clone(), old.ino))
            })
            .collect();
        self.modified.clear();
        self.replaced.clear();

        let cs = self.capture.capture_from(&mut self.journal)?;
        let snapshot = self
            .capture
            .last_snapshot()
            .map(Path::to_path_buf)
            .ok_or_else(|| anyhow!("capture retained no snapshot"))?;
        if self.trace {
            for op in &cs.ops {
                match op {
                    ChangeOp::Upsert { path, node } => {
                        eprintln!(
                            "  epoch {} upsert {:?} {}",
                            cs.epoch,
                            node.kind,
                            path.display()
                        );
                    }
                    ChangeOp::Remove { path, .. } => {
                        eprintln!("  epoch {} remove {}", cs.epoch, path.display());
                    }
                }
            }
        }
        let (node_changes, mat_updates, bytes) = self.publish(&cs, &snapshot);

        if let Some(old) = &previous_snapshot {
            self.detect_in_place(&cs, &in_place, old, &snapshot);
        }
        let update = if cs.stats.full_rescan {
            NodeUpdate::Replace(self.capture.state().map(|s| s.nodes).unwrap_or_default())
        } else {
            let mut changes = node_changes;
            for p in self.node_dirty.drain() {
                let n = self.capture.node_state(&p);
                changes.push((p, n));
            }
            for p in &cs.tracked {
                changes.push((p.clone(), self.capture.node_state(p)));
            }
            NodeUpdate::Delta(changes)
        };
        self.node_dirty.clear();
        self.state.save_epoch(
            self.capture.next_epoch(),
            &snapshot,
            cs.through,
            update,
            &mat_updates,
        )?;
        self.capture.release_retired()?;
        if let Ok(mut s) = self.status.lock() {
            s.epochs = s.epochs.saturating_add(1);
            s.ops_published = s.ops_published.saturating_add(cs.ops.len() as u64);
            s.bytes_published = s.bytes_published.saturating_add(bytes);
        }
        if !cs.ops.is_empty() {
            println!(
                "epoch {}: {} change(s), {} bytes published in {} ms (snapshot {} ms{})",
                cs.epoch,
                cs.ops.len(),
                bytes,
                t.elapsed().as_millis(),
                cs.stats.snapshot.as_millis(),
                if cs.stats.full_rescan {
                    ", full comparison"
                } else {
                    ""
                }
            );
        }
        self.leases.tick(QuickFS::record_clock_ms());
        Ok(())
    }

    /// Applies each operation through the record plane, in order.
    #[allow(
        clippy::type_complexity,
        reason = "the three outputs are persisted together with the epoch"
    )]
    fn publish(
        &mut self,
        cs: &Changeset,
        snapshot: &Path,
    ) -> (
        Vec<(PathBuf, Option<fs_native::capture::NodeState>)>,
        Vec<(PathBuf, Option<Materialized>)>,
        u64,
    ) {
        let mut nodes = Vec::new();
        let mut mats = Vec::new();
        let mut bytes = 0u64;
        let retry: Vec<PathBuf> = self.retry_removals.drain().collect();
        for path in retry {
            if self.fs.apply_local_remove(&path).is_err() {
                self.retry_removals.insert(path);
                continue;
            }
            self.forget_materialized(&path, &mut mats);
        }
        for op in &cs.ops {
            match op {
                ChangeOp::Upsert { path, node } => {
                    let applied = match &node.kind {
                        NodeKind::Dir => self.fs.apply_local_dir(path),
                        NodeKind::File => match std::fs::read(snapshot.join(path)) {
                            Ok(data) => {
                                bytes = bytes.saturating_add(data.len() as u64);
                                let mtime_ms = u64::try_from(node.mtime_ns.div_euclid(1_000_000))
                                    .unwrap_or_else(|_| QuickFS::record_clock_ms());
                                self.fs.apply_local_file(path, &data, mtime_ms)
                            }
                            Err(error) => Err(error.raw_os_error().unwrap_or(libc::EIO)),
                        },
                        NodeKind::Symlink(_) => {
                            if !self.symlink_warned {
                                eprintln!(
                                    "symlinks stay on this device: the record plane has no symlink record yet ({})",
                                    path.display()
                                );
                                self.symlink_warned = true;
                            }
                            nodes.push((path.clone(), self.capture.node_state(path)));
                            continue;
                        }
                    };
                    match applied {
                        Ok(()) => {
                            let captured = self.capture.node_state(path);
                            let entry = self.fs.entry_at(path);
                            let record = Materialized {
                                dir: node.kind == NodeKind::Dir,
                                hash: entry.as_ref().and_then(|e| e.hash).map(|h| *h.as_bytes()),
                                size: entry.as_ref().map_or(0, |e| e.size),
                                mtime_ms: entry.as_ref().map_or(0, |e| e.mtime_ms),
                                placeholder: false,
                                local: captured.as_ref().map(|n| LocalStat {
                                    ino: n.ino,
                                    size: n.size,
                                    mtime_ns: n.mtime_ns,
                                }),
                            };
                            self.materialized.insert(path.clone(), record.clone());
                            mats.push((path.clone(), Some(record)));
                            nodes.push((path.clone(), captured));
                        }
                        Err(errno) => {
                            eprintln!(
                                "publishing {} failed (errno {errno}); retrying next epoch",
                                path.display()
                            );
                            self.capture.invalidate(path);
                            self.retry_at = Instant::now().checked_add(RETRY_AFTER);
                            nodes.push((path.clone(), self.capture.node_state(path)));
                        }
                    }
                }
                ChangeOp::Remove { path, .. } => {
                    nodes.push((path.clone(), None));
                    if let Err(errno) = self.fs.apply_local_remove(path) {
                        eprintln!(
                            "publishing the removal of {} failed (errno {errno}); retrying next epoch",
                            path.display()
                        );
                        // Its materialised record stays, so the projection
                        // does not bring the path back meanwhile.
                        self.retry_removals.insert(path.clone());
                        self.retry_at = Instant::now().checked_add(RETRY_AFTER);
                        continue;
                    }
                    self.forget_materialized(path, &mut mats);
                }
            }
        }
        (nodes, mats, bytes)
    }

    /// Drops the materialised records of `path` and everything below it.
    fn forget_materialized(
        &mut self,
        path: &Path,
        mats: &mut Vec<(PathBuf, Option<Materialized>)>,
    ) {
        let below: Vec<PathBuf> = self
            .materialized
            .keys()
            .filter(|p| p.starts_with(path))
            .cloned()
            .collect();
        for p in below {
            self.materialized.remove(&p);
            mats.push((p, None));
        }
    }

    /// Marks files written in place (same inode, extents shared with the
    /// previous version) as lease-class.
    fn detect_in_place(
        &mut self,
        cs: &Changeset,
        candidates: &[(PathBuf, u64)],
        old_snapshot: &Path,
        new_snapshot: &Path,
    ) {
        let upserted: HashSet<&Path> = cs
            .ops
            .iter()
            .filter_map(|op| match op {
                ChangeOp::Upsert { path, node } if node.kind == NodeKind::File => {
                    Some(path.as_path())
                }
                _ => None,
            })
            .collect();
        for (path, old_ino) in candidates {
            if !upserted.contains(path.as_path()) || self.leases.lease_class().contains(path) {
                continue;
            }
            let Some(now) = self.capture.node_state(path) else {
                continue;
            };
            if now.ino != *old_ino {
                continue;
            }
            let (Ok(old), Ok(new)) = (
                std::fs::File::open(old_snapshot.join(path)),
                std::fs::File::open(new_snapshot.join(path)),
            ) else {
                continue;
            };
            let Ok(changed) = fs_native::extents::diff_files(&old, &new) else {
                continue;
            };
            let changed_bytes: u64 = changed.iter().map(|r| r.end.saturating_sub(r.start)).sum();
            if changed_bytes < now.size && self.leases.classify(path) {
                println!(
                    "{} is modified in place: it now takes a write lease per writing session",
                    path.display()
                );
                let _ = self.state.set_lease(
                    path,
                    Some(&StoredLease {
                        holder: [0; 32],
                        epoch: 0,
                        expires_ms: 0,
                    }),
                );
            }
        }
    }

    /// Materialises the record plane's current state.
    fn project(&mut self) {
        let tree = self.fs.tree_snapshot();
        let out = self.projector.project(&tree, &self.materialized);
        for rel in &out.written {
            if let Err(error) = self.capture.track(rel) {
                eprintln!("tracking {}: {error}", rel.display());
            }
            self.node_dirty.insert(rel.clone());
        }
        for rel in &out.removed {
            self.capture.untrack(rel);
            self.node_dirty.insert(rel.clone());
        }
        for (path, value) in &out.updates {
            match value {
                Some(m) => {
                    self.materialized.insert(path.clone(), m.clone());
                }
                None => {
                    self.materialized.remove(path);
                }
            }
        }
        let unavailable = self.projector.take_unavailable();
        if self.trace && out.waiting_for.is_some() && out.waiting_for != self.waiting_for {
            eprintln!(
                "  projection waits for {:?}: {}",
                out.waiting_for.as_deref().map(Path::display),
                unavailable.as_deref().unwrap_or("version not verified yet")
            );
        }
        self.waiting = out.waiting;
        self.waiting_for.clone_from(&out.waiting_for);
        if let Err(error) = self.state.set_materialized(&out.updates) {
            self.note_error(&error);
        }
        for e in &out.errors {
            eprintln!("materialising {e}");
        }
        if !out.written.is_empty() || !out.removed.is_empty() {
            println!(
                "applied remote changes: {} file(s), {} placeholder(s), {} removal(s), {} kept local",
                out.files,
                out.placeholders,
                out.removed.len(),
                out.skipped_dirty.len()
            );
        }
        if let Ok(mut s) = self.status.lock() {
            s.projections = s.projections.saturating_add(1);
            s.files_materialized = s.files_materialized.saturating_add(out.files as u64);
            s.placeholders_created = s
                .placeholders_created
                .saturating_add(out.placeholders as u64);
        }
    }
}

/// Deletes capture snapshots of this room other than `keep`: leftovers of a
/// run that stopped between cutting an epoch and persisting it.
fn remove_other_snapshots(epochs: &Path, keep: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(epochs) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.is_some_and(|k| k == path) {
            continue;
        }
        if fs_native::btrfs::is_subvolume(&path).unwrap_or(false) {
            let _ = fs_native::btrfs::delete_subvolume(&path);
        }
    }
}

/// Creates the room's subvolume if needed and decides who owns what the
/// daemon materialises: the configured owner, else the room directory's
/// (a new room takes the workspace mount point's owner).
fn prepare_room(layout: &StoreLayout, cfg: &NativeConfig) -> Result<(PathBuf, Option<(u32, u32)>)> {
    let fresh_room = !layout.room(&cfg.room).exists();
    let live = layout
        .ensure_room(&cfg.room)
        .with_context(|| format!("creating room {} on {}", cfg.room, cfg.store.display()))?;
    let owner_of = |p: &Path| std::fs::metadata(p).ok().map(|m| (m.uid(), m.gid()));
    if fresh_room && let Some((uid, gid)) = cfg.owner.or_else(|| owner_of(&cfg.mount_point)) {
        std::os::unix::fs::chown(&live, Some(uid), Some(gid))?;
    }
    let owner = cfg.owner.or_else(|| owner_of(&live));
    Ok((live, owner))
}

/// Starts the pre-content group and re-adopts every placeholder an earlier
/// run left, before anything can reach them.
fn start_placeholders(
    live: &Path,
    content: &Arc<Hydrator>,
    runtime: &tokio::runtime::Handle,
    status: &Mutex<PlaneStatus>,
) -> Result<Arc<Placeholders>> {
    let placeholders = Arc::new(
        Placeholders::start(
            Arc::new(ContentFetcher::new(
                Arc::clone(content),
                runtime.clone(),
                DEFAULT_PARALLEL_CHUNKS,
            )),
            HydratorConfig::default(),
        )
        .context("starting the pre-content group (needs CAP_SYS_ADMIN)")?,
    );
    let leftover_staging = project::remove_stale_staging(live)?;
    let mut adopted = 0usize;
    for path in project::find_placeholders(live)? {
        if placeholders.adopt(&path)? {
            adopted = adopted.saturating_add(1);
        }
    }
    if adopted > 0 || leftover_staging > 0 {
        println!(
            "Re-adopted {adopted} placeholder(s); removed {leftover_staging} stale staging file(s)"
        );
    }
    if let Ok(mut s) = status.lock() {
        s.placeholders_adopted = adopted;
    }
    Ok(placeholders)
}

/// Continues from the persisted epoch, or starts from scratch when there is
/// none or its snapshot is gone.
fn open_capture(
    live: &Path,
    epochs: &Path,
    state: &NativeState,
    persisted: Option<fs_native::capture::CaptureState>,
) -> Result<EpochCapture> {
    let mut cfg = CaptureConfig::new(live, epochs);
    cfg.hash_content = false;
    cfg.defer_release = true;
    let Some(resume) = persisted else {
        remove_other_snapshots(epochs, None);
        return Ok(EpochCapture::new(cfg)?);
    };
    let keep = resume.snapshot.clone();
    remove_other_snapshots(epochs, Some(&keep));
    match EpochCapture::resume(cfg.clone(), resume) {
        Ok(c) => Ok(c),
        Err(error) => {
            eprintln!("cannot resume the last epoch ({error}); comparing from scratch");
            state.clear_capture()?;
            Ok(EpochCapture::new(cfg)?)
        }
    }
}
