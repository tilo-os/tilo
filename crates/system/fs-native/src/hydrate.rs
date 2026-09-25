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
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::ops::Range;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
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

/// Supplies content bytes for placeholders.
pub trait Fetcher: Send + Sync + 'static {
    /// Returns exactly `len` bytes of `content` starting at `offset`.
    fn fetch(&self, content: &[u8; 32], offset: u64, len: u64) -> io::Result<Vec<u8>>;
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
}

impl Default for HydratorConfig {
    fn default() -> Self {
        Self {
            granule: 256 * 1024,
            workers: 8,
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
}

struct Shared {
    group: Group,
    fetcher: Arc<dyn Fetcher>,
    files: Mutex<HashMap<(u64, u64), FileState>>,
    changed: Condvar,
    stop: AtomicBool,
    cfg: HydratorConfig,
    stats: HydrationStats,
}

/// Serves pre-content events for the placeholders it created or adopted.
pub struct Hydrator {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

fn encode_residency(st: &FileState) -> Vec<u8> {
    let hex = st
        .content
        .iter()
        .fold(String::with_capacity(64), |mut h, b| {
            let _ = write!(h, "{b:02x}");
            h
        });
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
    /// Handles one pre-content event and returns the response to send.
    fn serve(&self, fd: BorrowedFd<'_>, range: Option<(u64, u64)>) -> u32 {
        let Ok(key) = key_of(fd) else {
            return fan::FAN_ALLOW;
        };
        let want = {
            let Ok(files) = self.files.lock() else {
                return fan::FAN_ALLOW;
            };
            let Some(st) = files.get(&key) else {
                self.stats.passthrough.fetch_add(1, Ordering::Relaxed);
                return fan::FAN_ALLOW;
            };
            let raw = match range {
                Some((offset, count)) if count > 0 => offset..offset.saturating_add(count),
                // No range (e.g. an open-time event): the whole file.
                _ => 0..st.size,
            };
            align_range(
                raw.start.min(st.size)..raw.end.min(st.size),
                self.cfg.granule,
                st.size,
            )
        };
        if self.fill(key, fd, &want).is_ok() {
            fan::FAN_ALLOW
        } else {
            self.stats.failures.fetch_add(1, Ordering::Relaxed);
            fan::deny_errno(libc::EIO)
        }
    }

    fn fill(&self, key: (u64, u64), fd: BorrowedFd<'_>, want: &Range<u64>) -> io::Result<()> {
        let poisoned = || io::Error::other("hydrator state poisoned");
        let mut files = self.files.lock().map_err(|_| poisoned())?;
        loop {
            let Some(st) = files.get_mut(&key) else {
                return Ok(());
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
            if claim.is_empty() {
                // Another worker is filling what we need; wait for it.
                files = self
                    .changed
                    .wait_timeout(files, Duration::from_millis(100))
                    .map_err(|_| poisoned())?
                    .0;
                continue;
            }
            for c in &claim {
                st.inflight.insert(c.clone());
            }
            let content = st.content;
            drop(files);
            let mut result = Ok(());
            let mut done = Vec::new();
            for c in &claim {
                let len = c.end.saturating_sub(c.start);
                let r = self
                    .fetcher
                    .fetch(&content, c.start, len)
                    .and_then(|bytes| {
                        if bytes.len() as u64 == len {
                            Ok(bytes)
                        } else {
                            Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "fetcher returned a short range",
                            ))
                        }
                    })
                    .and_then(|bytes| write_all_at(fd, &bytes, c.start));
                match r {
                    Ok(()) => {
                        self.stats.fills.fetch_add(1, Ordering::Relaxed);
                        self.stats.bytes_filled.fetch_add(len, Ordering::Relaxed);
                        done.push(c.clone());
                    }
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                }
            }
            files = self.files.lock().map_err(|_| poisoned())?;
            if let Some(st) = files.get_mut(&key) {
                for c in &claim {
                    st.inflight.remove(c.clone());
                }
                for d in done {
                    st.missing.remove(d);
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
            let _ = self
                .group
                .mark_fd(fan::FAN_MARK_REMOVE, fan::FAN_PRE_ACCESS, fd);
            let _ = sys::fremove_xattr(fd, RESIDENCY_XATTR);
        }
        Ok(())
    }
}

impl Hydrator {
    /// Starts serving. Needs `CAP_SYS_ADMIN` (pre-content groups).
    pub fn start(fetcher: Arc<dyn Fetcher>, cfg: HydratorConfig) -> io::Result<Self> {
        let event_flags = u32::try_from(libc::O_RDWR | libc::O_LARGEFILE).unwrap_or(2);
        let group = Group::new(
            fan::FAN_CLASS_PRE_CONTENT | fan::FAN_UNLIMITED_QUEUE | fan::FAN_UNLIMITED_MARKS,
            event_flags,
        )?;
        let workers = cfg.workers.max(1);
        let shared = Arc::new(Shared {
            group,
            fetcher,
            files: Mutex::new(HashMap::new()),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
            cfg,
            stats: HydrationStats::default(),
        });
        let (tx, rx) = mpsc::channel::<fan::Event>();
        let rx = Arc::new(Mutex::new(rx));
        let mut threads = Vec::with_capacity(workers.saturating_add(1));
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
                    let response = if ev.mask & fan::FAN_PRE_ACCESS == 0 {
                        fan::FAN_ALLOW
                    } else {
                        shared.serve(fd.as_fd(), ev.range())
                    };
                    if shared.group.respond(fd.as_raw_fd(), response).is_err() {
                        // Kernels without FAN_DENY_ERRNO accept only plain deny.
                        let _ = shared.group.respond(fd.as_raw_fd(), fan::FAN_DENY);
                    }
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

    fn register(&self, file: &File, st: FileState) -> io::Result<()> {
        let key = key_of(file.as_fd())?;
        let empty = st.missing.is_empty();
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
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
        let staging = dir.join(format!(
            ".tilo-staging-{}-{}",
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
            let st = FileState {
                content: spec.content,
                size: spec.size,
                missing: RangeSet::full(spec.size),
                inflight: RangeSet::new(),
            };
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
            sys::rename_noreplace(&staging, path)
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
        self.register(
            &file,
            FileState {
                content,
                size,
                missing,
                inflight: RangeSet::new(),
            },
        )?;
        Ok(true)
    }

    /// Returns a resident file to placeholder state by punching out its data.
    /// The caller guarantees that the content is durable elsewhere and that
    /// nothing has the file open.
    pub fn evict(&self, path: &Path, content: [u8; 32]) -> io::Result<()> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let size = file.metadata()?.len();
        let st = FileState {
            content,
            size,
            missing: RangeSet::full(size),
            inflight: RangeSet::new(),
        };
        sys::fset_xattr(&file, RESIDENCY_XATTR, &encode_residency(&st))?;
        self.register(&file, st)?;
        sys::punch_hole(&file, 0, size)
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
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }

    /// Descriptor of the pre-content group, for probes that need extra marks.
    pub fn group_fd(&self) -> BorrowedFd<'_> {
        self.shared.group.as_fd()
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
    fn residency_xattr_round_trips() {
        let mut missing = RangeSet::full(1000);
        missing.remove(0..100);
        missing.remove(500..600);
        let st = FileState {
            content: [0xAB; 32],
            size: 1000,
            missing: missing.clone(),
            inflight: RangeSet::new(),
        };
        let (content, size, back) = decode_residency(&encode_residency(&st)).unwrap();
        assert_eq!(content, [0xAB; 32]);
        assert_eq!(size, 1000);
        assert_eq!(back, missing);
    }

    #[test]
    fn fills_are_rounded_to_the_granule_and_clipped_to_the_file() {
        assert_eq!(align_range(10..20, 4096, 1 << 20), 0..4096);
        assert_eq!(align_range(5000..9000, 4096, 1 << 20), 4096..12288);
        assert_eq!(align_range(5000..9000, 4096, 6000), 4096..6000);
    }
}
