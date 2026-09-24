//! The change journal: a fanotify filesystem mark on the store volume,
//! turned into an ordered list of path-level changes.
//!
//! Writers are never intercepted. The journal only observes, in the order
//! the kernel queued the events, and that order is what peers apply
//! (`docs/architecture/storage.md`, §10, V3). The journal is an optimisation:
//! when it loses information (queue overflow, a directory that vanished
//! before its events were read) it asks for a rescan, and capture then
//! compares snapshots instead. The filesystem is the truth.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::fanotify::{self as fan, FidKind, Group};
use crate::sys::{self, FileHandle};

/// What happened to a path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum JournalOp {
    Create,
    Delete,
    /// Bytes were written. Rate-limited to the first write per file per
    /// batch; `CloseWrite` marks the end of a write session.
    Modify,
    CloseWrite,
    /// Mode, owner, times or xattrs changed.
    Attrib,
    /// Moved from `from` to the entry's path. Either side may lie outside
    /// the watched tree, in which case it is `None` and the move is a
    /// create (moved in) or a delete (moved out).
    Rename {
        from: Option<PathBuf>,
    },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct JournalEntry {
    /// Position in this device's journal. Strictly increasing.
    pub seq: u64,
    pub op: JournalOp,
    /// Path relative to the watched tree; `None` only for a rename out of it.
    pub path: Option<PathBuf>,
    pub is_dir: bool,
    pub pid: i32,
}

impl JournalEntry {
    /// Every path this entry touches (both sides of a rename).
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        let from = match &self.op {
            JournalOp::Rename { from } => from.as_deref(),
            _ => None,
        };
        self.path.as_deref().into_iter().chain(from)
    }
}

/// Why the journal cannot vouch for completeness.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RescanReason {
    /// The kernel dropped events (`FAN_Q_OVERFLOW`).
    Overflow,
    /// An event named a directory that no longer exists and was never seen.
    /// Where it was is unknown, so this fires for vanished directories
    /// anywhere on the volume; the daemon's own staging is pid-filtered and
    /// snapshot deletion raises no per-file events, so on a store volume
    /// this is rare.
    Unresolved,
}

/// Entries taken from the journal since the last [`ChangeJournal::take`].
#[derive(Clone, Default, Debug)]
pub struct JournalBatch {
    pub entries: Vec<JournalEntry>,
    pub rescan: Option<RescanReason>,
    /// Journal position after the last entry.
    pub through: u64,
}

#[derive(Clone, Debug)]
pub struct JournalConfig {
    /// The tree to journal, e.g. `<store>/live`. Events elsewhere on the
    /// volume (snapshots, staging) are dropped.
    pub watch: PathBuf,
    /// Use `FAN_UNLIMITED_QUEUE`. Off by default: an unbounded queue trades
    /// an overflow for unbounded kernel memory.
    pub unlimited_queue: bool,
    /// Events caused by these processes are dropped; the daemon lists
    /// itself so that applying remote changes is not captured as local.
    pub ignore_pids: Vec<i32>,
    /// Suppress repeated `FAN_MODIFY` of an open file with an ignore mark
    /// until it is closed or the batch is taken.
    pub rate_limit_modify: bool,
}

impl JournalConfig {
    pub fn new(watch: impl Into<PathBuf>) -> Self {
        Self {
            watch: watch.into(),
            unlimited_queue: false,
            ignore_pids: Vec::new(),
            rate_limit_modify: true,
        }
    }
}

const EVENTS: u64 = fan::FAN_CREATE
    | fan::FAN_DELETE
    | fan::FAN_RENAME
    | fan::FAN_MODIFY
    | fan::FAN_CLOSE_WRITE
    | fan::FAN_ATTRIB
    | fan::FAN_ONDIR;

pub struct ChangeJournal {
    group: Group,
    mount: OwnedFd,
    watch: PathBuf,
    cfg: JournalConfig,
    buf: Vec<u8>,
    entries: Vec<JournalEntry>,
    next_seq: u64,
    /// Why the journal cannot vouch for completeness, and the journal
    /// position (the next sequence number) at which that was noticed.
    rescan: Option<(RescanReason, u64)>,
    /// Barrier files not yet seen, by path, with their id.
    pending_barriers: HashMap<PathBuf, u64>,
    /// Journal position of every barrier seen: the last sequence number
    /// queued before it.
    barriers: HashMap<u64, u64>,
    next_barrier: u64,
    /// Last known path of every directory handle seen, for events whose
    /// directory is gone by the time they are read.
    dirs: HashMap<FileHandle, PathBuf>,
    /// Objects whose `FAN_MODIFY` was already journaled in this batch. With
    /// a descriptor, the kernel suppresses further events through an ignore
    /// mark; without one (the mark was refused), repeats are dropped here.
    muted: HashMap<FileHandle, Option<OwnedFd>>,
    events_read: u64,
    kernel_mutes: u64,
}

impl ChangeJournal {
    /// Marks the whole filesystem holding `store_root`. Needs
    /// `CAP_SYS_ADMIN` (filesystem marks) and `CAP_DAC_READ_SEARCH`
    /// (resolving file handles).
    pub fn open(store_root: &Path, cfg: JournalConfig) -> io::Result<Self> {
        let mut flags = fan::FAN_CLASS_NOTIF | fan::FAN_REPORT_DFID_NAME_TARGET | fan::FAN_NONBLOCK;
        if cfg.unlimited_queue {
            flags |= fan::FAN_UNLIMITED_QUEUE;
        }
        let group = Group::new(flags, u32::try_from(libc::O_RDONLY).unwrap_or(0))?;
        group.mark(
            fan::FAN_MARK_ADD | fan::FAN_MARK_FILESYSTEM,
            EVENTS,
            None,
            Some(store_root),
        )?;
        let mount = sys::open_dir(store_root)?;
        let watch = std::fs::canonicalize(&cfg.watch)?;
        Ok(Self {
            group,
            mount,
            watch,
            cfg,
            buf: vec![0u8; 256 * 1024],
            entries: Vec::new(),
            next_seq: 1,
            rescan: None,
            pending_barriers: HashMap::new(),
            barriers: HashMap::new(),
            next_barrier: 0,
            dirs: HashMap::new(),
            muted: HashMap::new(),
            events_read: 0,
            kernel_mutes: 0,
        })
    }

    /// The canonical path of the watched tree.
    pub fn watch(&self) -> &Path {
        &self.watch
    }

    /// Raw events read so far, for diagnostics.
    pub fn events_read(&self) -> u64 {
        self.events_read
    }

    /// How many write sessions the kernel suppressed with an ignore mark.
    /// Btrfs refuses inode ignore marks (`EXDEV`) in a subvolume whose fsid
    /// differs from the filesystem mark's, and then repeats are dropped in
    /// userspace instead.
    pub fn kernel_mutes(&self) -> u64 {
        self.kernel_mutes
    }

    /// Reads whatever is queued, waiting up to `timeout` for the first
    /// event. Returns the number of journal entries appended.
    pub fn pump(&mut self, timeout: Option<Duration>) -> io::Result<usize> {
        let before = self.entries.len();
        let mut wait = timeout;
        loop {
            let mut buf = std::mem::take(&mut self.buf);
            let events = self.group.read(&mut buf, wait);
            self.buf = buf;
            let events = events?;
            if events.is_empty() {
                break;
            }
            for ev in events {
                self.events_read = self.events_read.saturating_add(1);
                self.ingest(&ev);
            }
            wait = Some(Duration::ZERO);
        }
        Ok(self.entries.len().saturating_sub(before))
    }

    /// Pumps until no event at all has arrived for `idle`, or `limit`
    /// elapsed. Returns the number of journal entries appended.
    pub fn pump_until_idle(&mut self, idle: Duration, limit: Duration) -> io::Result<usize> {
        let start = Instant::now();
        let mut total = 0usize;
        while start.elapsed() < limit {
            let before = self.events_read;
            total = total.saturating_add(self.pump(Some(idle))?);
            if self.events_read == before {
                break;
            }
        }
        Ok(total)
    }

    /// Takes the entries journaled so far and re-arms modify tracking, so
    /// every batch sees at least the first write to each file.
    pub fn take(&mut self) -> JournalBatch {
        self.rearm_modify();
        JournalBatch {
            entries: std::mem::take(&mut self.entries),
            rescan: self.rescan.take().map(|(r, _)| r),
            through: self.next_seq.saturating_sub(1),
        }
    }

    /// Entries journaled but not yet taken.
    pub fn pending(&self) -> &[JournalEntry] {
        &self.entries
    }

    fn rearm_modify(&mut self) {
        for fd in self.muted.drain().filter_map(|(_, fd)| fd) {
            // Best effort: an evicted mark is already gone.
            let _ = self.group.mark_fd(
                fan::FAN_MARK_REMOVE | fan::FAN_MARK_IGNORE,
                fan::FAN_MODIFY,
                fd.as_fd(),
            );
        }
    }

    /// Places a barrier in the event stream: creates and removes a uniquely
    /// named file in `dir`, which must be on the marked filesystem (outside
    /// the watched tree, e.g. the epoch directory). fanotify queues events
    /// in the syscall that causes them, so every change that completed
    /// before this call is queued before the barrier's event.
    pub fn barrier(&mut self, dir: &Path) -> io::Result<u64> {
        let id = self.next_barrier;
        self.next_barrier = self.next_barrier.saturating_add(1);
        let path = std::fs::canonicalize(dir)?
            .join(format!(".tilo-journal-barrier-{}-{id}", std::process::id()));
        self.pending_barriers.insert(path.clone(), id);
        std::fs::File::create(&path)?;
        std::fs::remove_file(&path)?;
        Ok(id)
    }

    fn barrier_position(&mut self, id: u64, timeout: Duration) -> io::Result<u64> {
        let start = Instant::now();
        loop {
            if let Some(pos) = self.barriers.get(&id) {
                return Ok(*pos);
            }
            if start.elapsed() > timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "journal barrier was never observed",
                ));
            }
            self.pump(Some(Duration::from_millis(10)))?;
        }
    }

    /// Takes the batch for a snapshot taken between barriers `before` and
    /// `after`.
    ///
    /// Everything queued before `after` is returned. Entries queued between
    /// the two barriers may or may not be in the snapshot, so they are also
    /// kept for the next batch: examining a path twice is cheap, missing one
    /// loses a change. A rescan request noticed after `before` is likewise
    /// kept for the next batch too.
    pub fn take_epoch(&mut self, before: u64, after: u64) -> io::Result<JournalBatch> {
        let a = self.barrier_position(before, Duration::from_secs(10))?;
        let b = self.barrier_position(after, Duration::from_secs(10))?;
        self.barriers.remove(&before);
        self.barriers.remove(&after);
        self.rearm_modify();
        let all = std::mem::take(&mut self.entries);
        let (taken, later): (Vec<_>, Vec<_>) = all.into_iter().partition(|e| e.seq <= b);
        self.entries = taken
            .iter()
            .filter(|e| e.seq > a)
            .cloned()
            .chain(later)
            .collect();
        let rescan = match self.rescan {
            Some((reason, pos)) if pos <= b.saturating_add(1) => {
                if pos <= a.saturating_add(1) {
                    self.rescan = None;
                }
                Some(reason)
            }
            _ => None,
        };
        Ok(JournalBatch {
            entries: taken,
            rescan,
            through: b,
        })
    }

    fn note_rescan(&mut self, reason: RescanReason) {
        // Keep the earliest position: everything after it is suspect.
        if self.rescan.is_none() {
            self.rescan = Some((reason, self.next_seq));
        }
    }

    fn push(&mut self, op: JournalOp, path: Option<PathBuf>, is_dir: bool, pid: i32) {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.entries.push(JournalEntry {
            seq,
            op,
            path,
            is_dir,
            pid,
        });
    }

    fn ingest(&mut self, ev: &fan::Event) {
        if ev.is_overflow() {
            self.note_rescan(RescanReason::Overflow);
            return;
        }
        if !self.pending_barriers.is_empty()
            && ev.mask & fan::FAN_CREATE != 0
            && let Some((dir, name)) = ev.fid(FidKind::Parent)
            && let Some(abs) = self.resolve_dir(dir, name.map(OsStr::new))
            && let Some(id) = self.pending_barriers.remove(&abs)
        {
            self.barriers.insert(id, self.next_seq.saturating_sub(1));
            return;
        }
        if self.cfg.ignore_pids.contains(&ev.pid) {
            return;
        }
        let is_dir = ev.mask & fan::FAN_ONDIR != 0;
        if ev.mask & fan::FAN_RENAME != 0 {
            self.ingest_rename(ev, is_dir);
        }
        let Some((dir, name)) = ev.fid(FidKind::Parent) else {
            return;
        };
        let Some(abs) = self.resolve_dir(dir, name.map(OsStr::new)) else {
            self.note_rescan(RescanReason::Unresolved);
            return;
        };
        let Some(rel) = self.relative(&abs) else {
            return;
        };
        let object = ev.fid(FidKind::Object).map(|(h, _)| h.clone());
        if is_dir
            && ev.mask & fan::FAN_CREATE != 0
            && let Some(h) = &object
        {
            self.dirs.insert(h.clone(), abs);
        }
        for (bit, op) in [
            (fan::FAN_CREATE, JournalOp::Create),
            (fan::FAN_ATTRIB, JournalOp::Attrib),
            (fan::FAN_MODIFY, JournalOp::Modify),
            (fan::FAN_CLOSE_WRITE, JournalOp::CloseWrite),
            (fan::FAN_DELETE, JournalOp::Delete),
        ] {
            if ev.mask & bit == 0 {
                continue;
            }
            if let Some(h) = &object {
                if bit == fan::FAN_MODIFY {
                    if self.cfg.rate_limit_modify && self.muted.contains_key(h) {
                        continue;
                    }
                    self.mute_modify(h);
                } else if bit == fan::FAN_CLOSE_WRITE {
                    self.unmute_modify(h);
                }
            }
            self.push(op, Some(rel.clone()), is_dir, ev.pid);
        }
    }

    fn ingest_rename(&mut self, ev: &fan::Event, is_dir: bool) {
        let side = |this: &mut Self, kind| -> Result<Option<PathBuf>, ()> {
            let Some((dir, name)) = ev.fid(kind) else {
                return Ok(None);
            };
            let abs = this.resolve_dir(dir, name.map(OsStr::new)).ok_or(())?;
            Ok(this.relative(&abs))
        };
        let (Ok(from), Ok(to)) = (
            side(self, FidKind::RenameFrom),
            side(self, FidKind::RenameTo),
        ) else {
            self.note_rescan(RescanReason::Unresolved);
            return;
        };
        if is_dir {
            // Every cached path below the moved directory is now stale.
            self.dirs.clear();
        }
        match (from, to) {
            (None, None) => {}
            (Some(from), None) => self.push(JournalOp::Delete, Some(from), is_dir, ev.pid),
            (from, Some(to)) => self.push(JournalOp::Rename { from }, Some(to), is_dir, ev.pid),
        }
    }

    /// Absolute path of `name` inside the directory `dir`.
    fn resolve_dir(&mut self, dir: &FileHandle, name: Option<&OsStr>) -> Option<PathBuf> {
        let base = match dir.open(self.mount.as_fd(), libc::O_PATH) {
            Ok(fd) => {
                let p = sys::fd_path(fd.as_fd()).ok()?;
                self.dirs.insert(dir.clone(), p.clone());
                p
            }
            Err(_) => self.dirs.get(dir)?.clone(),
        };
        Some(match name {
            Some(n) => base.join(n),
            None => base,
        })
    }

    fn relative(&self, abs: &Path) -> Option<PathBuf> {
        abs.strip_prefix(&self.watch).ok().map(Path::to_path_buf)
    }

    fn mute_modify(&mut self, object: &FileHandle) {
        if !self.cfg.rate_limit_modify || self.muted.contains_key(object) {
            return;
        }
        let fd = object
            .open(self.mount.as_fd(), libc::O_PATH)
            .ok()
            .filter(|fd| {
                self.group
                    .mark_fd(
                        fan::FAN_MARK_ADD | fan::FAN_MARK_IGNORE_SURV | fan::FAN_MARK_EVICTABLE,
                        fan::FAN_MODIFY,
                        fd.as_fd(),
                    )
                    .is_ok()
            });
        if fd.is_some() {
            self.kernel_mutes = self.kernel_mutes.saturating_add(1);
        }
        self.muted.insert(object.clone(), fd);
    }

    fn unmute_modify(&mut self, object: &FileHandle) {
        if let Some(fd) = self.muted.remove(object).flatten() {
            let _ = self.group.mark_fd(
                fan::FAN_MARK_REMOVE | fan::FAN_MARK_IGNORE,
                fan::FAN_MODIFY,
                fd.as_fd(),
            );
        }
    }
}
