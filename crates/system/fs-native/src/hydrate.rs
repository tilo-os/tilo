//! Placeholder files and on-demand hydration through fanotify pre-content
//! events.
//!
//! A placeholder is a real inode with the exact size, mode, owner and mtime
//! of the version it stands for, and no data: a sparse file. `stat`,
//! `readdir` and `ls -l` never involve the daemon. Each placeholder carries
//! an inode mark for `FAN_PRE_ACCESS`, so the first read, write or `mmap` of
//! a range that is not resident blocks that one caller until the range has
//! been fetched and written in place. Resident files carry no mark, so their
//! I/O never generates an event at all.
//!
//! Writes into the file happen through the event's own descriptor, which
//! the kernel opens with `FMODE_NONOTIFY`: filling a placeholder raises no
//! further events and is invisible to the change journal.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::ops::Range;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use crate::fanotify::{self as fan, Group};
use crate::ranges::RangeSet;
use crate::sys;

/// Residency marker of a placeholder or partially resident file. Present
/// while any range is missing; removed once the file is fully resident.
/// The value is `P <size> <content-id-hex> <resident ranges>`.
pub const RESIDENCY_XATTR: &str = "trusted.tilo.residency";

/// Name prefix of files being prepared next to their final path. They are
/// renamed into place once complete and are never captured.
pub const STAGING_PREFIX: &str = ".tilo-staging-";

/// How a gated file treats writes.
///
/// Every access is classified on its own ([`crate::access`]): a process
/// that holds the file open read-write and reads it is a reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteGate {
    /// Refuse writes with this errno (`EBUSY` while another device holds
    /// the file's write lease), and refuse opening it for writing, so a
    /// program that falls back to a read-only open when a read-write one
    /// fails (`SQLite` does) reads normally and reports a read-only file
    /// when it tries to write.
    Deny(i32),
    /// Ask the [`WritePolicy`] at the first write, which may grant it
    /// (acquiring a lease) or refuse it.
    Ask,
}

/// Decides writes to files gated with [`WriteGate::Ask`].
pub trait WritePolicy: Send + Sync + 'static {
    /// Called on the first access by a writer of `file` (`(st_dev, st_ino)`)
    /// while it is gated. `Err(errno)` refuses the access with that errno.
    fn admit_writer(&self, file: (u64, u64)) -> Result<(), i32>;
}

/// Supplies content bytes for placeholders.
pub trait Fetcher: Send + Sync + 'static {
    /// Returns exactly `len` bytes of `content` starting at `offset`.
    fn fetch(&self, content: &[u8; 32], offset: u64, len: u64) -> io::Result<Vec<u8>>;

    /// Writes bytes `range` of `content` into `dest` at the same offsets.
    /// `ahead` marks read-ahead for a sequential reader rather than a
    /// blocked access. The default fetches and writes; a fetcher whose
    /// content lives in files can clone extents instead.
    fn fill(
        &self,
        content: &[u8; 32],
        range: Range<u64>,
        dest: BorrowedFd<'_>,
        ahead: bool,
    ) -> io::Result<()> {
        let _ = ahead;
        let len = range.end.saturating_sub(range.start);
        let bytes = self.fetch(content, range.start, len)?;
        if bytes.len() as u64 != len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "fetcher returned a short range",
            ));
        }
        write_all_at(dest, &bytes, range.start)
    }
}

/// Everything a placeholder needs to be indistinguishable from the real file
/// in `stat`.
#[derive(Clone, Debug)]
pub struct PlaceholderSpec {
    pub content: [u8; 32],
    pub size: u64,
    pub mode: u32,
    pub mtime: SystemTime,
    /// Owner to materialise as; `None` keeps the creating process's.
    pub owner: Option<(u32, u32)>,
}

#[derive(Clone, Debug)]
pub struct HydratorConfig {
    /// Fills are rounded out to this alignment, so a sequential reader
    /// raises one event per granule rather than per `read(2)`.
    pub granule: u64,
    /// Concurrent fills. A reader of one file never waits for a fill of
    /// another as long as a worker is free.
    pub workers: usize,
    /// Largest read-ahead window. A reader that continues where its last
    /// fill ended gets the next window fetched in the background; the
    /// window doubles on every sequential fill up to this size and resets
    /// on a seek. `0` disables read-ahead.
    pub readahead_max: u64,
    /// Background read-ahead fills in flight at once.
    pub readahead_workers: usize,
}

impl Default for HydratorConfig {
    fn default() -> Self {
        Self {
            granule: 256 * 1024,
            workers: 8,
            readahead_max: 64 * 1024 * 1024,
            readahead_workers: 4,
        }
    }
}

/// Counters for observability and tests.
#[derive(Default, Debug)]
pub struct HydrationStats {
    pub events: AtomicU64,
    pub fills: AtomicU64,
    pub bytes_filled: AtomicU64,
    pub failures: AtomicU64,
    /// Events that asked for ranges already resident (or unknown files).
    pub passthrough: AtomicU64,
    /// Bytes fetched ahead of a sequential reader.
    pub readahead_bytes: AtomicU64,
    /// Accesses denied because the file is gated against writers.
    pub gated_denials: AtomicU64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Residency {
    Resident,
    Partial { resident: u64, size: u64 },
    Placeholder,
}

struct FileState {
    content: [u8; 32],
    size: u64,
    missing: RangeSet,
    inflight: RangeSet,
    /// End of the last foreground fill, to recognise sequential readers.
    seq_end: u64,
    /// Current read-ahead window; `0` until a reader proves sequential.
    window: u64,
    /// Furthest offset read-ahead was already scheduled up to.
    ahead_end: u64,
    /// The mtime the file should show. Filling writes bytes, which moves
    /// the mtime; it is put back so a placeholder that becomes resident
    /// looks exactly like the version it stands for (`ls -l`, `git status`,
    /// make). A change made by someone else between fills is kept.
    mtime: Option<libc::timespec>,
}

impl FileState {
    fn new(content: [u8; 32], size: u64, missing: RangeSet) -> Self {
        Self {
            content,
            size,
            missing,
            inflight: RangeSet::new(),
            seq_end: 0,
            window: 0,
            ahead_end: 0,
            mtime: None,
        }
    }
}

struct ReadAhead {
    key: (u64, u64),
    fd: OwnedFd,
    range: Range<u64>,
}

struct Shared {
    group: Group,
    fetcher: Arc<dyn Fetcher>,
    files: Mutex<HashMap<(u64, u64), FileState>>,
    /// Files whose writers are refused or must be admitted first.
    gates: Mutex<HashMap<(u64, u64), WriteGate>>,
    policy: Mutex<Option<Arc<dyn WritePolicy>>>,
    /// Descriptors this process holds for events and read-ahead. They are
    /// open read-write on the files they name and must not make this
    /// process look like a writer.
    own_fds: Mutex<std::collections::HashSet<i32>>,
    changed: Condvar,
    stop: AtomicBool,
    cfg: HydratorConfig,
    stats: HydrationStats,
    readahead: Mutex<Option<mpsc::Sender<ReadAhead>>>,
}

/// Serves pre-content events for the placeholders it created or adopted.
pub struct Hydrator {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

fn encode_residency(st: &FileState) -> Vec<u8> {
    let hex = hex::encode(st.content);
    let mut resident = RangeSet::full(st.size);
    for r in st.missing.iter() {
        resident.remove(r.clone());
    }
    let ranges: Vec<String> = resident
        .iter()
        .map(|r| format!("{}-{}", r.start, r.end))
        .collect();
    format!("P {} {} {}", st.size, hex, ranges.join(",")).into_bytes()
}

fn decode_residency(raw: &[u8]) -> Option<([u8; 32], u64, RangeSet)> {
    let text = std::str::from_utf8(raw).ok()?;
    let mut parts = text.split(' ');
    if parts.next()? != "P" {
        return None;
    }
    let size: u64 = parts.next()?.parse().ok()?;
    let hex = parts.next()?;
    let mut content = [0u8; 32];
    for (i, b) in content.iter_mut().enumerate() {
        let at = i.checked_mul(2)?;
        *b = u8::from_str_radix(hex.get(at..at.checked_add(2)?)?, 16).ok()?;
    }
    let mut missing = RangeSet::full(size);
    for r in parts
        .next()
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
    {
        let (a, b) = r.split_once('-')?;
        missing.remove(a.parse().ok()?..b.parse().ok()?);
    }
    Some((content, size, missing))
}

/// Whether `tid` is one of this process's threads.
fn is_own_thread(tid: i32) -> bool {
    Path::new("/proc/self/task").join(tid.to_string()).exists()
}

fn key_of(fd: BorrowedFd<'_>) -> io::Result<(u64, u64)> {
    // SAFETY: an all-zero `stat` is valid for the kernel to fill.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is writable; `fd` is open.
    sys::cvt(unsafe { libc::fstat(fd.as_raw_fd(), &raw mut st) })?;
    Ok((st.st_dev, st.st_ino))
}

fn align_range(r: Range<u64>, granule: u64, size: u64) -> Range<u64> {
    let g = granule.max(1);
    let start = r.start.checked_div(g).unwrap_or(0).saturating_mul(g);
    let end = r.end.div_ceil(g).saturating_mul(g).min(size);
    start..end.max(start)
}

fn write_all_at(fd: BorrowedFd<'_>, mut data: &[u8], mut offset: u64) -> io::Result<()> {
    while !data.is_empty() {
        let off = libc::off_t::try_from(offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "offset out of range"))?;
        // SAFETY: `data` is readable for its length; `fd` is open for write.
        let n = unsafe { libc::pwrite(fd.as_raw_fd(), data.as_ptr().cast(), data.len(), off) };
        let Ok(n) = usize::try_from(n) else {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        };
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "short write"));
        }
        data = data.get(n..).unwrap_or_default();
        offset = offset.saturating_add(n as u64);
    }
    Ok(())
}

impl Shared {
    fn gated(&self, key: (u64, u64)) -> Option<WriteGate> {
        self.gates.lock().ok()?.get(&key).copied()
    }

    /// The verdict for a writer of a gated file.
    fn admit(&self, gate: WriteGate, key: (u64, u64)) -> Result<(), i32> {
        match gate {
            WriteGate::Deny(errno) => Err(errno),
            WriteGate::Ask => {
                let policy = self.policy.lock().ok().and_then(|p| p.clone());
                policy.map_or(Ok(()), |p| p.admit_writer(key))
            }
        }
    }

    fn own_fd(&self, fd: i32, held: bool) {
        if let Ok(mut fds) = self.own_fds.lock() {
            if held {
                fds.insert(fd);
            } else {
                fds.remove(&fd);
            }
        }
    }

    /// Whether the access by thread `tid` writes `key`: from the call it
    /// is blocked in, else from its descriptor table.
    fn writes(&self, tid: i32, key: (u64, u64)) -> bool {
        match crate::access::classify(tid, key) {
            crate::access::Access::Write => true,
            crate::access::Access::Read => false,
            crate::access::Access::Unknown => self.process_writes(tid, key),
        }
    }

    /// The verdict on opening a gated file: a `Deny` gate refuses opening
    /// it for writing. An open that cannot be classified is let through;
    /// its first write is still refused.
    fn admit_open(&self, fd: BorrowedFd<'_>, tid: i32) -> u32 {
        let Ok(key) = key_of(fd) else {
            return fan::FAN_ALLOW;
        };
        match self.gated(key) {
            Some(WriteGate::Deny(errno))
                if !is_own_thread(tid)
                    && crate::access::classify(tid, key) == crate::access::Access::Write =>
            {
                self.stats.gated_denials.fetch_add(1, Ordering::Relaxed);
                fan::deny_errno(errno)
            }
            _ => fan::FAN_ALLOW,
        }
    }

    /// Whether the process of thread `pid` holds `key` open for writing:
    /// the fallback when the call itself cannot be read. A writer is a
    /// process with the file open `O_WRONLY` or `O_RDWR`.
    fn process_writes(&self, pid: i32, key: (u64, u64)) -> bool {
        let own = is_own_thread(pid);
        let skip = if own {
            self.own_fds.lock().map(|g| g.clone()).unwrap_or_default()
        } else {
            std::collections::HashSet::new()
        };
        let fds = Path::new("/proc").join(pid.to_string()).join("fd");
        let Ok(entries) = fs::read_dir(&fds) else {
            return false;
        };
        for entry in entries.flatten() {
            let fd_num = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<i32>().ok());
            if fd_num.is_some_and(|n| skip.contains(&n)) {
                continue;
            }
            let Ok(meta) = fs::metadata(entry.path()) else {
                continue;
            };
            if (meta.dev(), meta.ino()) != key {
                continue;
            }
            let info = Path::new("/proc")
                .join(pid.to_string())
                .join("fdinfo")
                .join(entry.file_name());
            let Ok(text) = fs::read_to_string(info) else {
                continue;
            };
            let flags = text
                .lines()
                .find_map(|l| l.strip_prefix("flags:"))
                .and_then(|v| u32::from_str_radix(v.trim(), 8).ok())
                .unwrap_or(0);
            let mode = flags & u32::try_from(libc::O_ACCMODE).unwrap_or(3);
            if mode != u32::try_from(libc::O_RDONLY).unwrap_or(0) {
                return true;
            }
        }
        false
    }

    /// Handles one pre-content event and returns the response to send.
    fn serve(&self, fd: BorrowedFd<'_>, range: Option<(u64, u64)>, pid: i32) -> u32 {
        let Ok(key) = key_of(fd) else {
            return fan::FAN_ALLOW;
        };
        if let Some(gate) = self.gated(key)
            && self.writes(pid, key)
            && let Err(errno) = self.admit(gate, key)
        {
            self.stats.gated_denials.fetch_add(1, Ordering::Relaxed);
            return fan::deny_errno(errno);
        }
        let (want, ahead) = {
            let Ok(mut files) = self.files.lock() else {
                return fan::FAN_ALLOW;
            };
            let Some(st) = files.get_mut(&key) else {
                self.stats.passthrough.fetch_add(1, Ordering::Relaxed);
                return fan::FAN_ALLOW;
            };
            let raw = match range {
                Some((offset, count)) if count > 0 => offset..offset.saturating_add(count),
                // No range (e.g. an open-time event): the whole file.
                _ => 0..st.size,
            };
            let want = align_range(
                raw.start.min(st.size)..raw.end.min(st.size),
                self.cfg.granule,
                st.size,
            );
            let ahead = plan_readahead(&self.cfg, st, &want);
            (want, ahead)
        };
        if let Some(range) = ahead
            && let Ok(dup) = fd.try_clone_to_owned()
            && let Ok(tx) = self.readahead.lock()
            && let Some(tx) = tx.as_ref()
        {
            self.own_fd(dup.as_raw_fd(), true);
            let raw = dup.as_raw_fd();
            if tx
                .send(ReadAhead {
                    key,
                    fd: dup,
                    range,
                })
                .is_err()
            {
                self.own_fd(raw, false);
            }
        }
        match self.fill(key, fd, &want, true) {
            Ok(_) => fan::FAN_ALLOW,
            Err(error) => {
                self.stats.failures.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "placeholder {}:{} bytes {}..{} unavailable: {error}",
                    key.0, key.1, want.start, want.end
                );
                fan::deny_errno(libc::EIO)
            }
        }
    }

    /// Fetches the claimed ranges and writes them in place. Returns the
    /// ranges written, and the first error, which stops the rest.
    fn fetch_claims(
        &self,
        content: &[u8; 32],
        fd: BorrowedFd<'_>,
        claim: &[Range<u64>],
        ahead: bool,
    ) -> (Vec<Range<u64>>, io::Result<()>) {
        let mut done = Vec::new();
        for c in claim {
            let len = c.end.saturating_sub(c.start);
            let r = self.fetcher.fill(content, c.clone(), fd, ahead);
            match r {
                Ok(()) => {
                    self.stats.fills.fetch_add(1, Ordering::Relaxed);
                    self.stats.bytes_filled.fetch_add(len, Ordering::Relaxed);
                    done.push(c.clone());
                }
                Err(e) => return (done, Err(e)),
            }
        }
        (done, Ok(()))
    }

    /// Fetches and writes the missing parts of `want`. With `block`, ranges
    /// another worker is filling are waited for; without it they are left
    /// to that worker.
    fn fill(
        &self,
        key: (u64, u64),
        fd: BorrowedFd<'_>,
        want: &Range<u64>,
        block: bool,
    ) -> io::Result<u64> {
        let poisoned = || io::Error::other("hydrator state poisoned");
        let mut files = self.files.lock().map_err(|_| poisoned())?;
        let mut filled = 0u64;
        loop {
            let Some(st) = files.get_mut(&key) else {
                return Ok(filled);
            };
            let missing = st.missing.intersect(want);
            if missing.is_empty() {
                break;
            }
            let mut claim = Vec::new();
            for m in &missing {
                let mut free = RangeSet::new();
                free.insert(m.clone());
                for busy in st.inflight.intersect(m) {
                    free.remove(busy);
                }
                claim.extend(free.iter().cloned());
            }
            if claim.is_empty() && !block {
                return Ok(filled);
            }
            if claim.is_empty() {
                // Another worker is filling what we need; wait for it.
                files = self
                    .changed
                    .wait_timeout(files, Duration::from_millis(100))
                    .map_err(|_| poisoned())?
                    .0;
                continue;
            }
            if st.inflight.is_empty()
                && let Ok(now) = sys::fmtime(fd)
                && st
                    .mtime
                    .is_none_or(|t| t.tv_sec != now.tv_sec || t.tv_nsec != now.tv_nsec)
            {
                // Nobody is filling, so a different mtime is someone else's.
                st.mtime = Some(now);
            }
            for c in &claim {
                st.inflight.insert(c.clone());
            }
            let content = st.content;
            drop(files);
            let (done, result) = self.fetch_claims(&content, fd, &claim, !block);
            for d in &done {
                filled = filled.saturating_add(d.end.saturating_sub(d.start));
            }
            files = self.files.lock().map_err(|_| poisoned())?;
            if let Some(st) = files.get_mut(&key) {
                for c in &claim {
                    st.inflight.remove(c.clone());
                }
                let wrote = !done.is_empty();
                for d in done {
                    st.missing.remove(d);
                }
                if wrote
                    && st.inflight.is_empty()
                    && let Some(t) = st.mtime
                {
                    let _ = sys::set_fmtime(fd, t);
                }
                // Persist progress so a restart never refetches over local
                // writes to ranges that were already resident.
                let _ = sys::fset_xattr_fd(fd, RESIDENCY_XATTR, &encode_residency(st));
            }
            self.changed.notify_all();
            result?;
        }
        let resident = files.get(&key).is_some_and(|st| st.missing.is_empty());
        if resident {
            files.remove(&key);
            drop(files);
            if self.gated(key).is_none() {
                let _ = self
                    .group
                    .mark_fd(fan::FAN_MARK_REMOVE, fan::FAN_PRE_ACCESS, fd);
            }
            let _ = sys::fremove_xattr(fd, RESIDENCY_XATTR);
        }
        Ok(filled)
    }
}

/// Starts the background fillers that fetch ahead of sequential readers.
fn spawn_readahead(shared: &Arc<Shared>, workers: usize, threads: &mut Vec<JoinHandle<()>>) {
    if workers == 0 {
        return;
    }
    let (tx, rx) = mpsc::channel::<ReadAhead>();
    if let Ok(mut slot) = shared.readahead.lock() {
        *slot = Some(tx);
    }
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..workers {
        let shared = Arc::clone(shared);
        let rx = Arc::clone(&rx);
        threads.push(std::thread::spawn(move || {
            loop {
                let job = match rx.lock() {
                    Ok(g) => g.recv_timeout(Duration::from_millis(50)),
                    Err(_) => return,
                };
                let job = match job {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if shared.stop.load(Ordering::Relaxed) {
                            return;
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                };
                // A failed read-ahead costs nothing: the reader's own event
                // fetches the range again.
                if let Ok(n) = shared.fill(job.key, job.fd.as_fd(), &job.range, false) {
                    shared.stats.readahead_bytes.fetch_add(n, Ordering::Relaxed);
                }
                shared.own_fd(job.fd.as_raw_fd(), false);
            }
        }));
    }
}

/// Decides what to fetch ahead of a reader whose fill is `want`.
fn plan_readahead(
    cfg: &HydratorConfig,
    st: &mut FileState,
    want: &Range<u64>,
) -> Option<Range<u64>> {
    if cfg.readahead_max == 0 || want.is_empty() {
        return None;
    }
    let sequential = st.seq_end > 0 && want.start <= st.seq_end && want.end >= st.seq_end;
    if sequential {
        let floor = cfg.granule.saturating_mul(4);
        st.window = st
            .window
            .saturating_mul(2)
            .max(floor)
            .min(cfg.readahead_max);
    } else {
        st.window = 0;
        st.ahead_end = 0;
    }
    st.seq_end = want.end;
    if st.window == 0 {
        return None;
    }
    // Top up only once the reader has used half of what is ahead of it, so
    // read-ahead is fetched in large pieces rather than one granule per
    // foreground event.
    if st.ahead_end > want.end && st.ahead_end.saturating_sub(want.end) >= st.window / 2 {
        return None;
    }
    let start = want.end.max(st.ahead_end);
    let end = want.end.saturating_add(st.window).min(st.size);
    if start >= end {
        return None;
    }
    st.ahead_end = end;
    Some(start..end)
}

impl Hydrator {
    /// Starts serving. Needs `CAP_SYS_ADMIN` (pre-content groups).
    pub fn start(fetcher: Arc<dyn Fetcher>, cfg: HydratorConfig) -> io::Result<Self> {
        let event_flags = u32::try_from(libc::O_RDWR | libc::O_LARGEFILE).unwrap_or(2);
        // Thread ids, so a gated access is classified by the call of the
        // thread that made it.
        let group = Group::new(
            fan::FAN_CLASS_PRE_CONTENT
                | fan::FAN_UNLIMITED_QUEUE
                | fan::FAN_UNLIMITED_MARKS
                | fan::FAN_REPORT_TID,
            event_flags,
        )?;
        let workers = cfg.workers.max(1);
        let readahead_workers = if cfg.readahead_max == 0 {
            0
        } else {
            cfg.readahead_workers.max(1)
        };
        let shared = Arc::new(Shared {
            group,
            fetcher,
            files: Mutex::new(HashMap::new()),
            gates: Mutex::new(HashMap::new()),
            policy: Mutex::new(None),
            own_fds: Mutex::new(std::collections::HashSet::new()),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
            cfg,
            stats: HydrationStats::default(),
            readahead: Mutex::new(None),
        });
        let (tx, rx) = mpsc::channel::<fan::Event>();
        let rx = Arc::new(Mutex::new(rx));
        let mut threads =
            Vec::with_capacity(workers.saturating_add(readahead_workers).saturating_add(1));
        spawn_readahead(&shared, readahead_workers, &mut threads);
        for _ in 0..workers {
            let shared = Arc::clone(&shared);
            let rx = Arc::clone(&rx);
            threads.push(std::thread::spawn(move || {
                loop {
                    let ev = match rx.lock() {
                        Ok(g) => g.recv(),
                        Err(_) => return,
                    };
                    let Ok(ev) = ev else { return };
                    shared.stats.events.fetch_add(1, Ordering::Relaxed);
                    let Some(fd) = ev.fd.as_ref() else { continue };
                    let response = if ev.mask & fan::FAN_PRE_ACCESS != 0 {
                        shared.serve(fd.as_fd(), ev.range(), ev.pid)
                    } else if ev.mask & fan::FAN_OPEN_PERM != 0 {
                        shared.admit_open(fd.as_fd(), ev.pid)
                    } else {
                        fan::FAN_ALLOW
                    };
                    if shared.group.respond(fd.as_raw_fd(), response).is_err() {
                        // Kernels without FAN_DENY_ERRNO accept only plain deny.
                        let _ = shared.group.respond(fd.as_raw_fd(), fan::FAN_DENY);
                    }
                    shared.own_fd(fd.as_raw_fd(), false);
                }
            }));
        }
        let reader = Arc::clone(&shared);
        threads.push(std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while !reader.stop.load(Ordering::Relaxed) {
                match reader.group.read(&mut buf, Some(Duration::from_millis(50))) {
                    Ok(events) => {
                        for ev in events {
                            if let Some(fd) = ev.fd.as_ref() {
                                reader.own_fd(fd.as_raw_fd(), true);
                            }
                            if tx.send(ev).is_err() {
                                return;
                            }
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }));
        Ok(Self { shared, threads })
    }

    pub fn stats(&self) -> &HydrationStats {
        &self.shared.stats
    }

    fn register(&self, file: &File, mut st: FileState) -> io::Result<()> {
        let key = key_of(file.as_fd())?;
        let empty = st.missing.is_empty();
        st.mtime = sys::fmtime(file.as_fd()).ok();
        self.shared
            .files
            .lock()
            .map_err(|_| io::Error::other("hydrator state poisoned"))?
            .insert(key, st);
        if !empty {
            self.shared.group.mark_fd(
                fan::FAN_MARK_ADD | fan::FAN_MARK_INODE,
                fan::FAN_PRE_ACCESS,
                file.as_fd(),
            )?;
        }
        Ok(())
    }

    /// Creates a placeholder at `path`, which must not exist.
    ///
    /// The file is prepared under a hidden name in the same directory, marked,
    /// and only then renamed into place, so no reader can ever see it
    /// without its mark.
    pub fn create_placeholder(&self, path: &Path, spec: &PlaceholderSpec) -> io::Result<()> {
        self.place(path, spec, false)
    }

    /// Atomically replaces whatever is at `path` with a placeholder for a
    /// new version. A process that has the old file open keeps reading the
    /// old inode, as with any rename over an open file.
    pub fn replace_with_placeholder(&self, path: &Path, spec: &PlaceholderSpec) -> io::Result<()> {
        self.place(path, spec, true)
    }

    fn place(&self, path: &Path, spec: &PlaceholderSpec, replace: bool) -> io::Result<()> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
        let staging = dir.join(format!(
            "{STAGING_PREFIX}{}-{}",
            std::process::id(),
            String::from_utf8_lossy(name.as_bytes())
        ));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)?;
        let result = (|| {
            file.set_len(spec.size)?;
            let st = FileState::new(spec.content, spec.size, RangeSet::full(spec.size));
            sys::fset_xattr(&file, RESIDENCY_XATTR, &encode_residency(&st))?;
            if let Some((uid, gid)) = spec.owner {
                std::os::unix::fs::fchown(&file, Some(uid), Some(gid))?;
            }
            file.set_permissions(fs::Permissions::from_mode(spec.mode & 0o7777 & !0o4000))?;
            let since = spec
                .mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            sys::futimens(
                &file,
                libc::timespec {
                    tv_sec: libc::time_t::try_from(since.as_secs()).unwrap_or(0),
                    tv_nsec: libc::c_long::from(since.subsec_nanos()),
                },
            )?;
            self.register(&file, st)?;
            if replace {
                fs::rename(&staging, path)
            } else {
                sys::rename_noreplace(&staging, path)
            }
        })();
        if result.is_err() {
            let _ = fs::remove_file(&staging);
        }
        result
    }

    /// Takes over a placeholder left by a previous run, from its residency
    /// xattr. Returns `false` when the file is fully resident.
    pub fn adopt(&self, path: &Path) -> io::Result<bool> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let Some(raw) = sys::fget_xattr(file.as_fd(), RESIDENCY_XATTR)? else {
            return Ok(false);
        };
        let (content, size, mut missing) = decode_residency(&raw)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad residency xattr"))?;
        missing.truncate(file.metadata()?.len().min(size));
        self.register(&file, FileState::new(content, size, missing))?;
        Ok(true)
    }

    /// Returns a resident file to placeholder state by punching out its data.
    /// The caller guarantees that the content is durable elsewhere and that
    /// nothing has the file open.
    pub fn evict(&self, path: &Path, content: [u8; 32]) -> io::Result<()> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let size = file.metadata()?.len();
        let st = FileState::new(content, size, RangeSet::full(size));
        sys::fset_xattr(&file, RESIDENCY_XATTR, &encode_residency(&st))?;
        self.register(&file, st)?;
        sys::punch_hole(&file, 0, size)
    }

    /// Whether `path` is a placeholder or partially resident file, from its
    /// residency xattr. Reading the xattr never hydrates anything.
    pub fn is_placeholder(path: &Path) -> bool {
        matches!(sys::get_xattr(path, RESIDENCY_XATTR), Ok(Some(_)))
    }

    /// Refuses writers of the file at `path` with `errno` (for example
    /// `EBUSY` while another device holds its write lease). Readers keep
    /// reading; a process that has the file open for writing gets `errno`
    /// from the access that needs the bytes, `mmap` included.
    pub fn gate_writes(&self, path: &Path, errno: i32) -> io::Result<()> {
        self.gate(path, WriteGate::Deny(errno)).map(|_| ())
    }

    /// Installs the policy asked about files gated with [`WriteGate::Ask`].
    pub fn set_write_policy(&self, policy: Arc<dyn WritePolicy>) {
        if let Ok(mut slot) = self.shared.policy.lock() {
            *slot = Some(policy);
        }
    }

    /// The `(st_dev, st_ino)` a [`WritePolicy`] is asked about for `path`.
    pub fn file_key(path: &Path) -> io::Result<(u64, u64)> {
        let meta = fs::symlink_metadata(path)?;
        Ok((meta.dev(), meta.ino()))
    }

    /// Gates writers of the file at `path`. Returns its key.
    pub fn gate(&self, path: &Path, gate: WriteGate) -> io::Result<(u64, u64)> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let key = key_of(file.as_fd())?;
        self.shared
            .gates
            .lock()
            .map_err(|_| io::Error::other("hydrator state poisoned"))?
            .insert(key, gate);
        let (add, remove) = match gate {
            WriteGate::Deny(_) => (fan::FAN_PRE_ACCESS | fan::FAN_OPEN_PERM, 0),
            WriteGate::Ask => (fan::FAN_PRE_ACCESS, fan::FAN_OPEN_PERM),
        };
        self.shared
            .group
            .mark_fd(fan::FAN_MARK_ADD | fan::FAN_MARK_INODE, add, file.as_fd())?;
        if remove != 0 {
            // Nothing to remove unless the file was denied before.
            let _ = self
                .shared
                .group
                .mark_fd(fan::FAN_MARK_REMOVE, remove, file.as_fd());
        }
        Ok(key)
    }

    /// Lets writers of `path` through again.
    pub fn ungate_writes(&self, path: &Path) -> io::Result<()> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let key = key_of(file.as_fd())?;
        self.shared
            .gates
            .lock()
            .map_err(|_| io::Error::other("hydrator state poisoned"))?
            .remove(&key);
        let placeholder = self
            .shared
            .files
            .lock()
            .map_err(|_| io::Error::other("hydrator state poisoned"))?
            .contains_key(&key);
        let events = if placeholder {
            fan::FAN_OPEN_PERM
        } else {
            fan::FAN_PRE_ACCESS | fan::FAN_OPEN_PERM
        };
        let _ = self
            .shared
            .group
            .mark_fd(fan::FAN_MARK_REMOVE, events, file.as_fd());
        Ok(())
    }

    pub fn residency(&self, path: &Path) -> io::Result<Residency> {
        let meta = fs::metadata(path)?;
        let files = self
            .shared
            .files
            .lock()
            .map_err(|_| io::Error::other("hydrator state poisoned"))?;
        Ok(match files.get(&(meta.dev(), meta.ino())) {
            None => Residency::Resident,
            Some(st) if st.missing.total() >= st.size => Residency::Placeholder,
            Some(st) => Residency::Partial {
                resident: st.size.saturating_sub(st.missing.total()),
                size: st.size,
            },
        })
    }

    /// Stops serving. Pending and future accesses to placeholders are
    /// answered by the kernel once the group is closed.
    pub fn stop(mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Ok(mut tx) = self.shared.readahead.lock() {
            tx.take();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Hydrator {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn residency_xattrs_round_trip_and_fills_align_to_granules() {
        // Phase `residency_xattr_round_trips`.
        {
            let mut missing = RangeSet::full(1000);
            missing.remove(0..100);
            missing.remove(500..600);
            let st = FileState::new([0xAB; 32], 1000, missing.clone());
            let (content, size, back) = decode_residency(&encode_residency(&st)).unwrap();
            assert_eq!(content, [0xAB; 32]);
            assert_eq!(size, 1000);
            assert_eq!(back, missing);
        }
        // Phase `fills_are_rounded_to_the_granule_and_clipped_to_the_file`.
        {
            assert_eq!(align_range(10..20, 4096, 1 << 20), 0..4096);
            assert_eq!(align_range(5000..9000, 4096, 1 << 20), 4096..12288);
            assert_eq!(align_range(5000..9000, 4096, 6000), 4096..6000);
        }
    }
}

#[cfg(test)]
mod readahead_tests {
    use super::*;

    #[test]
    fn readahead_window_doubles_for_sequential_fills_and_resets_on_a_seek() {
        let cfg = HydratorConfig {
            granule: 1024,
            readahead_max: 64 * 1024,
            ..HydratorConfig::default()
        };
        let mut st = FileState::new([0; 32], 1 << 20, RangeSet::full(1 << 20));
        // First fill: nothing is known about the reader yet.
        assert_eq!(plan_readahead(&cfg, &mut st, &(0..1024)), None);
        // Continues where it ended: fetch a window ahead.
        assert_eq!(
            plan_readahead(&cfg, &mut st, &(1024..2048)),
            Some(2048..6144)
        );
        // Continues again: the window doubles, nothing is scheduled twice.
        assert_eq!(
            plan_readahead(&cfg, &mut st, &(2048..3072)),
            Some(6144..11264)
        );
        // A seek resets it.
        assert_eq!(plan_readahead(&cfg, &mut st, &(500_000..501_024)), None);
        // The window is capped.
        let mut last = None;
        let mut at = 501_024;
        for _ in 0..10 {
            // A read inside what is already ahead schedules nothing; keep the
            // last window that was scheduled.
            if let Some(scheduled) = plan_readahead(&cfg, &mut st, &(at..at + 1024)) {
                last = Some(scheduled);
            }
            at += 1024;
        }
        let r = last.expect("a sequential reader schedules read-ahead");
        assert!(
            r.end.saturating_sub(at) <= 64 * 1024,
            "window capped: {r:?}"
        );
        // Disabled.
        let off = HydratorConfig {
            readahead_max: 0,
            ..cfg
        };
        assert_eq!(plan_readahead(&off, &mut st, &(at..at + 1024)), None);
    }
}
