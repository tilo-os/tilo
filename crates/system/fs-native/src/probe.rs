//! Capability probes for the Tier 1 data plane and the tier they select.
//!
//! Every probe exercises the feature on the store volume and reports what
//! the kernel actually did; nothing is inferred from a version string. The
//! kernel facts are K1–K6.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::fanotify::{self as fan, Group};
use crate::journal::{ChangeJournal, JournalConfig, JournalOp};
use crate::{btrfs, extents, store, sys};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Supported,
    Unsupported,
    /// The probe could not run here (for example without privileges).
    Unknown,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub id: &'static str,
    pub title: &'static str,
    pub status: Status,
    pub detail: String,
}

enum Outcome {
    Yes(String),
    No(String),
    Unknown(String),
}

fn run(id: &'static str, title: &'static str, f: impl FnOnce() -> Outcome) -> Check {
    let (status, detail) = match f() {
        Outcome::Yes(d) => (Status::Supported, d),
        Outcome::No(d) => (Status::Unsupported, d),
        Outcome::Unknown(d) => (Status::Unknown, d),
    };
    Check {
        id,
        title,
        status,
        detail,
    }
}

/// Snapshot latency measured by K5.
#[derive(Clone, Debug, Default)]
pub struct SnapshotCost {
    pub files: usize,
    pub dirty_bytes: u64,
    pub samples: Vec<Duration>,
}

impl SnapshotCost {
    /// The `pct`-th percentile (nearest rank) of the samples.
    pub fn percentile(&self, pct: usize) -> Duration {
        let mut v = self.samples.clone();
        v.sort();
        let idx = v
            .len()
            .saturating_sub(1)
            .saturating_mul(pct.min(100))
            .saturating_add(50)
            / 100;
        v.get(idx).copied().unwrap_or_default()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// Native tree on the store volume, fanotify journal, pre-content
    /// placeholders.
    Native,
    /// FUSE with passthrough for resident files.
    Passthrough,
    /// Plain FUSE.
    Fuse,
    Unavailable,
}

/// How placeholders are made safe for every access path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaceholderMode {
    /// Ranges hydrate on the access that needs them, `mmap` included.
    OnAccess,
    /// `mmap` or copy paths are not covered by range events: hydrate the
    /// whole file before it can be opened.
    WholeFileOnOpen,
}

#[derive(Clone, Debug)]
pub struct TierDecision {
    pub tier: Tier,
    pub placeholders: Option<PlaceholderMode>,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct CapabilityReport {
    pub kernel: String,
    pub store: PathBuf,
    pub checks: Vec<Check>,
    pub snapshot_cost: Option<SnapshotCost>,
}

impl CapabilityReport {
    pub fn get(&self, id: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.id == id)
    }

    fn ok(&self, id: &str) -> bool {
        self.get(id).is_some_and(|c| c.status == Status::Supported)
    }

    pub fn decide(&self) -> TierDecision {
        let required = [
            "volume",
            "fanotify-fs-mark",
            "journal",
            "pre-content",
            "snapshot",
            "fiemap-diff",
            "reflink",
        ];
        let missing: Vec<String> = required
            .iter()
            .filter(|id| !self.ok(id))
            .map(|id| {
                let why = self
                    .get(id)
                    .map_or_else(|| "not probed".to_owned(), |c| c.detail.clone());
                format!("{id}: {why}")
            })
            .collect();
        if missing.is_empty() {
            let copies_covered = self.ok("K3");
            let mmap_covered = self.ok("K2");
            let mode = if mmap_covered && copies_covered {
                PlaceholderMode::OnAccess
            } else {
                PlaceholderMode::WholeFileOnOpen
            };
            let mut reasons = vec!["all Tier 1 prerequisites hold".to_owned()];
            if mode == PlaceholderMode::WholeFileOnOpen {
                reasons.push("K2/K3 incomplete: placeholders hydrate whole on open".to_owned());
            }
            return TierDecision {
                tier: Tier::Native,
                placeholders: Some(mode),
                reasons,
            };
        }
        let fuse = Path::new("/dev/fuse").exists();
        let tier = if !fuse {
            Tier::Unavailable
        } else if self.ok("fuse-passthrough") {
            Tier::Passthrough
        } else {
            Tier::Fuse
        };
        TierDecision {
            tier,
            placeholders: None,
            reasons: missing,
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "kernel {}  store {}",
            self.kernel,
            self.store.display()
        );
        for c in &self.checks {
            let mark = match c.status {
                Status::Supported => "yes",
                Status::Unsupported => "no ",
                Status::Unknown => "?  ",
            };
            let _ = writeln!(out, "  [{mark}] {:<16} {:<44} {}", c.id, c.title, c.detail);
        }
        let d = self.decide();
        let _ = writeln!(out, "tier: {:?} placeholders: {:?}", d.tier, d.placeholders);
        for r in d.reasons {
            let _ = writeln!(out, "  - {r}");
        }
        out
    }
}

/// Sizes for the snapshot-cost probe (K5).
#[derive(Clone, Copy, Debug)]
pub struct K5Params {
    pub files: usize,
    pub dirty_bytes: u64,
    pub samples: usize,
}

impl Default for K5Params {
    fn default() -> Self {
        Self {
            files: 10_000,
            dirty_bytes: 64 * 1024 * 1024,
            samples: 7,
        }
    }
}

/// Runs every probe against the store volume mounted at `store_root`.
pub fn probe(store_root: &Path, k5: Option<K5Params>) -> io::Result<CapabilityReport> {
    let scratch = store_root.join(format!(".probe-{}", std::process::id()));
    if scratch.exists() {
        let _ = remove_scratch(&scratch);
    }
    let subvol = btrfs::create_subvolume(&scratch);
    if subvol.is_err() {
        fs::create_dir_all(&scratch)?;
    }
    let admin = sys::has_capability(sys::CAP_SYS_ADMIN);
    let mut checks = Vec::new();
    let root = store_root.to_path_buf();

    checks.push(run(
        "volume",
        "dedicated Btrfs store volume",
        || match store::check_dedicated(&root) {
            Ok(()) => Outcome::Yes("own superblock, mounted at its top subvolume".into()),
            Err(e) => Outcome::No(e.to_string()),
        },
    ));
    let dedicated = checks.last().is_some_and(|c| c.status == Status::Supported);
    let need_admin = |what: &str| Outcome::Unknown(format!("{what} needs CAP_SYS_ADMIN"));

    checks.push(run(
        "fanotify-fid",
        "unprivileged FID group, inode marks",
        || probe_fid_group(&scratch),
    ));
    checks.push(run(
        "fanotify-fs-mark",
        "filesystem mark with DFID_NAME + RENAME",
        || {
            if !admin {
                return need_admin("a filesystem mark");
            }
            if !dedicated {
                return Outcome::Unknown(
                    "refusing a filesystem-wide mark on a shared volume".into(),
                );
            }
            probe_fs_mark(&root)
        },
    ));
    checks.push(run(
        "journal",
        "create/modify/rename/delete journaled in order",
        || {
            if !admin || !dedicated {
                return Outcome::Unknown("needs a filesystem mark".into());
            }
            probe_journal(&root, &scratch)
        },
    ));
    checks.push(run(
        "modify-ignore",
        "inode ignore mark under a filesystem mark",
        || {
            if !admin || !dedicated {
                return Outcome::Unknown("needs a filesystem mark".into());
            }
            probe_modify_ignore(&root, &scratch)
        },
    ));
    checks.push(run(
        "pre-content",
        "FAN_PRE_ACCESS inode mark with range info",
        || {
            if !admin {
                return need_admin("a pre-content group");
            }
            probe_pre_content(&scratch)
        },
    ));
    checks.push(run(
        "snapshot",
        "read-only subvolume snapshot + delete",
        || probe_snapshot(&scratch),
    ));
    checks.push(run(
        "fiemap-diff",
        "extent diff finds exactly the rewritten range",
        || probe_fiemap_diff(&scratch),
    ));
    checks.push(run("reflink", "FICLONE / FICLONERANGE", || {
        probe_reflink(&scratch)
    }));
    let snapshot_cost = kernel_checks(&mut checks, &root, &scratch, admin, dedicated, k5);
    checks.push(run(
        "fuse-passthrough",
        "FUSE passthrough (Tier 2)",
        probe_fuse_passthrough,
    ));

    let _ = remove_scratch(&scratch);
    Ok(CapabilityReport {
        kernel: sys::kernel_release(),
        store: store_root.to_path_buf(),
        checks,
        snapshot_cost,
    })
}

/// The kernel facts K1–K6.
fn kernel_checks(
    checks: &mut Vec<Check>,
    root: &Path,
    scratch: &Path,
    admin: bool,
    dedicated: bool,
    k5: Option<K5Params>,
) -> Option<SnapshotCost> {
    let need_admin = |what: &str| Outcome::Unknown(format!("{what} needs CAP_SYS_ADMIN"));
    let mut snapshot_cost = None;
    checks.push(run(
        "K1",
        "evictable ignore mark suppresses FAN_PRE_ACCESS",
        || {
            if !admin || !dedicated {
                return Outcome::Unknown("needs a filesystem-wide pre-content mark".into());
            }
            probe_k1(root, scratch)
        },
    ));
    checks.push(run(
        "K2",
        "mmap of a placeholder raises a pre-content event",
        || {
            if !admin {
                return need_admin("a pre-content group");
            }
            probe_k2(scratch)
        },
    ));
    checks.push(run(
        "K3",
        "splice/sendfile/copy_file_range/FICLONE covered",
        || {
            if !admin {
                return need_admin("a pre-content group");
            }
            probe_k3(scratch)
        },
    ));
    checks.push(run("K4", "FAN_DENY_ERRNO returns EIO/EBUSY/EROFS", || {
        if !admin {
            return need_admin("a pre-content group");
        }
        probe_k4(scratch)
    }));
    checks.push(run("K5", "epoch snapshot latency with dirty data", || {
        let Some(params) = k5 else {
            return Outcome::Unknown("not measured (pass K5 parameters)".into());
        };
        match probe_k5(scratch, params) {
            Ok(cost) => {
                let p50 = cost.percentile(50);
                let p99 = cost.percentile(99);
                let detail = format!(
                    "{} files, {} MiB dirty: p50 {:.1} ms, p99 {:.1} ms",
                    cost.files,
                    cost.dirty_bytes / (1024 * 1024),
                    p50.as_secs_f64() * 1e3,
                    p99.as_secs_f64() * 1e3
                );
                snapshot_cost = Some(cost);
                if p99 < Duration::from_millis(50) {
                    Outcome::Yes(detail)
                } else {
                    Outcome::No(format!("{detail} (gate: p99 < 50 ms)"))
                }
            }
            Err(e) => Outcome::No(e.to_string()),
        }
    }));
    checks.push(run(
        "K6",
        "mount marks report directory-entry events",
        || {
            if !admin {
                return need_admin("a mount mark");
            }
            probe_k6(root)
        },
    ));
    snapshot_cost
}

fn remove_scratch(scratch: &Path) -> io::Result<()> {
    // Snapshots made by probes live inside the scratch subvolume.
    if let Ok(entries) = fs::read_dir(scratch) {
        for e in entries.flatten() {
            let p = e.path();
            if btrfs::is_subvolume(&p).unwrap_or(false) {
                let _ = btrfs::delete_subvolume(&p);
            }
        }
    }
    if btrfs::is_subvolume(scratch).unwrap_or(false) {
        btrfs::delete_subvolume(scratch)
    } else {
        fs::remove_dir_all(scratch)
    }
}

fn errno_name(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::EPERM) => "EPERM".into(),
        Some(libc::EINVAL) => "EINVAL".into(),
        Some(libc::EXDEV) => "EXDEV".into(),
        Some(libc::EOPNOTSUPP) => "EOPNOTSUPP".into(),
        Some(libc::ENODEV) => "ENODEV".into(),
        Some(libc::EIO) => "EIO".into(),
        Some(libc::EBUSY) => "EBUSY".into(),
        Some(libc::EROFS) => "EROFS".into(),
        Some(libc::EACCES) => "EACCES".into(),
        Some(n) => format!("errno {n}"),
        None => e.to_string(),
    }
}

fn write_file(path: &Path, len: usize, byte: u8) -> io::Result<File> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    f.write_all(&vec![byte; len])?;
    f.sync_all()?;
    Ok(f)
}

fn sparse_file(path: &Path, len: u64) -> io::Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    f.set_len(len)?;
    Ok(f)
}

fn probe_fid_group(scratch: &Path) -> Outcome {
    let group = match Group::new(fan::FAN_CLASS_NOTIF | fan::FAN_REPORT_DFID_NAME_TARGET, 0) {
        Ok(g) => g,
        Err(e) => return Outcome::No(format!("fanotify_init: {}", errno_name(&e))),
    };
    match group.mark(
        fan::FAN_MARK_ADD,
        fan::FAN_CREATE | fan::FAN_DELETE | fan::FAN_RENAME | fan::FAN_ONDIR,
        None,
        Some(scratch),
    ) {
        Ok(()) => Outcome::Yes("directory inode marks report child names".into()),
        Err(e) => Outcome::No(format!("inode mark: {}", errno_name(&e))),
    }
}

fn probe_fs_mark(root: &Path) -> Outcome {
    let group = match Group::new(fan::FAN_CLASS_NOTIF | fan::FAN_REPORT_DFID_NAME_TARGET, 0) {
        Ok(g) => g,
        Err(e) => return Outcome::No(format!("fanotify_init: {}", errno_name(&e))),
    };
    let mask = fan::FAN_CREATE
        | fan::FAN_DELETE
        | fan::FAN_RENAME
        | fan::FAN_MODIFY
        | fan::FAN_CLOSE_WRITE
        | fan::FAN_ATTRIB
        | fan::FAN_ONDIR;
    match group.mark(
        fan::FAN_MARK_ADD | fan::FAN_MARK_FILESYSTEM,
        mask,
        None,
        Some(root),
    ) {
        Ok(()) => Outcome::Yes("placed on the store superblock".into()),
        Err(e) => Outcome::No(format!("fanotify_mark: {}", errno_name(&e))),
    }
}

/// Whether the journal can suppress repeated `FAN_MODIFY` of one file with
/// an inode ignore mark, for files in the top-level subvolume and in a room
/// subvolume.
fn probe_modify_ignore(root: &Path, scratch: &Path) -> Outcome {
    let group = match Group::new(fan::FAN_CLASS_NOTIF | fan::FAN_REPORT_DFID_NAME_TARGET, 0) {
        Ok(g) => g,
        Err(e) => return Outcome::No(format!("fanotify_init: {}", errno_name(&e))),
    };
    if let Err(e) = group.mark(
        fan::FAN_MARK_ADD | fan::FAN_MARK_FILESYSTEM,
        fan::FAN_MODIFY,
        None,
        Some(root),
    ) {
        return Outcome::No(format!("filesystem mark: {}", errno_name(&e)));
    }
    let top = root.join(format!(".probe-ignore-{}", std::process::id()));
    let sub = scratch.join("ignore");
    let mut results = Vec::new();
    let mut all = true;
    for (label, path) in [("top-level subvolume", &top), ("room subvolume", &sub)] {
        let file = match write_file(path, 16, 0) {
            Ok(f) => f,
            Err(e) => return Outcome::No(e.to_string()),
        };
        let evictable = group.mark_fd(
            fan::FAN_MARK_ADD | fan::FAN_MARK_IGNORE_SURV | fan::FAN_MARK_EVICTABLE,
            fan::FAN_MODIFY,
            file.as_fd(),
        );
        let plain = group.mark_fd(
            fan::FAN_MARK_ADD | fan::FAN_MARK_IGNORE_SURV,
            fan::FAN_MODIFY,
            file.as_fd(),
        );
        let show = |r: &io::Result<()>| r.as_ref().map_or_else(errno_name, |()| "ok".to_owned());
        all &= evictable.is_ok();
        results.push(format!(
            "{label}: evictable {}, plain {}",
            show(&evictable),
            show(&plain)
        ));
    }
    let _ = fs::remove_file(&top);
    let detail = results.join("; ");
    if all {
        Outcome::Yes(detail)
    } else {
        Outcome::No(detail)
    }
}

fn probe_journal(root: &Path, scratch: &Path) -> Outcome {
    let dir = scratch.join("journal");
    if let Err(e) = fs::create_dir_all(&dir) {
        return Outcome::No(e.to_string());
    }
    let mut cfg = JournalConfig::new(&dir);
    cfg.rate_limit_modify = true;
    let mut j = match ChangeJournal::open(root, cfg) {
        Ok(j) => j,
        Err(e) => return Outcome::No(format!("open: {}", errno_name(&e))),
    };
    let work = || -> io::Result<()> {
        fs::create_dir(dir.join("d"))?;
        let mut f = File::create(dir.join("d/a"))?;
        for _ in 0..100 {
            f.write_all(&[1u8; 4096])?;
        }
        drop(f);
        fs::rename(dir.join("d/a"), dir.join("d/b"))?;
        fs::set_permissions(
            dir.join("d/b"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )?;
        fs::remove_file(dir.join("d/b"))?;
        Ok(())
    };
    if let Err(e) = work() {
        return Outcome::No(e.to_string());
    }
    if let Err(e) = j.pump_until_idle(Duration::from_millis(100), Duration::from_secs(2)) {
        return Outcome::No(e.to_string());
    }
    let batch = j.take();
    let got: Vec<String> = batch
        .entries
        .iter()
        .map(|e| {
            let p = e
                .path
                .as_ref()
                .map_or(String::new(), |p| p.display().to_string());
            match &e.op {
                JournalOp::Rename { from } => format!(
                    "rename {}->{p}",
                    from.as_ref()
                        .map_or(String::new(), |f| f.display().to_string())
                ),
                op => format!("{op:?} {p}").to_lowercase(),
            }
        })
        .collect();
    let expected = [
        "create d",
        "create d/a",
        "modify d/a",
        "closewrite d/a",
        "rename d/a->d/b",
        "attrib d/b",
        "delete d/b",
    ];
    let modifies = got.iter().filter(|g| g.starts_with("modify")).count();
    let mut it = got.iter();
    let in_order = expected.iter().all(|want| it.any(|g| g == want));
    if in_order {
        Outcome::Yes(format!(
            "{} entries in writer order; {modifies} modify event(s) for 100 writes",
            got.len()
        ))
    } else {
        Outcome::No(format!("unexpected journal: {got:?}"))
    }
}

/// A pre-content group whose events are logged and answered with a
/// configurable response.
/// `(mask, inode, range)` of every event a [`Recorder`] answered.
type EventLog = Arc<Mutex<Vec<(u64, u64, Option<(u64, u64)>)>>>;

struct Recorder {
    group: Arc<Group>,
    log: EventLog,
    response: Arc<AtomicU32>,
    respond_errors: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    fn start() -> io::Result<Self> {
        let group = Arc::new(Group::new(
            fan::FAN_CLASS_PRE_CONTENT | fan::FAN_NONBLOCK,
            u32::try_from(libc::O_RDWR | libc::O_LARGEFILE).unwrap_or(2),
        )?);
        let log = Arc::new(Mutex::new(Vec::new()));
        let response = Arc::new(AtomicU32::new(fan::FAN_ALLOW));
        let respond_errors = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (g, l, r, re, s) = (
            Arc::clone(&group),
            Arc::clone(&log),
            Arc::clone(&response),
            Arc::clone(&respond_errors),
            Arc::clone(&stop),
        );
        let thread = std::thread::spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            while !s.load(Ordering::Relaxed) {
                let Ok(events) = g.read(&mut buf, Some(Duration::from_millis(20))) else {
                    continue;
                };
                for ev in events {
                    let Some(fd) = ev.fd.as_ref() else { continue };
                    let ino = fs::metadata(format!("/proc/self/fd/{}", fd.as_raw_fd()))
                        .map_or(0, |m| m.ino());
                    if let Ok(mut log) = l.lock() {
                        log.push((ev.mask, ino, ev.range()));
                    }
                    let want = if ev.mask & fan::FAN_PRE_ACCESS != 0 {
                        r.load(Ordering::Relaxed)
                    } else {
                        fan::FAN_ALLOW
                    };
                    if let Err(e) = g.respond(fd.as_raw_fd(), want) {
                        if let Ok(mut errs) = re.lock() {
                            errs.push(errno_name(&e));
                        }
                        let _ = g.respond(fd.as_raw_fd(), fan::FAN_DENY);
                    }
                }
            }
        });
        Ok(Self {
            group,
            log,
            response,
            respond_errors,
            stop,
            thread: Some(thread),
        })
    }

    fn events_for(&self, ino: u64) -> Vec<Option<(u64, u64)>> {
        self.log
            .lock()
            .map(|l| {
                l.iter()
                    .filter(|(m, i, _)| *i == ino && m & fan::FAN_PRE_ACCESS != 0)
                    .map(|(_, _, r)| *r)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn mark_inode(&self, file: &File) -> io::Result<()> {
        self.group.mark_fd(
            fan::FAN_MARK_ADD | fan::FAN_MARK_INODE,
            fan::FAN_PRE_ACCESS,
            file.as_fd(),
        )
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn probe_pre_content(scratch: &Path) -> Outcome {
    let rec = match Recorder::start() {
        Ok(r) => r,
        Err(e) => return Outcome::No(format!("fanotify_init(PRE_CONTENT): {}", errno_name(&e))),
    };
    let path = scratch.join("pc");
    let file = match sparse_file(&path, 1 << 20) {
        Ok(f) => f,
        Err(e) => return Outcome::No(e.to_string()),
    };
    if let Err(e) = rec.mark_inode(&file) {
        return Outcome::No(format!("FAN_PRE_ACCESS mark: {}", errno_name(&e)));
    }
    let ino = file.metadata().map_or(0, |m| m.ino());
    let reader = match File::open(&path) {
        Ok(f) => f,
        Err(e) => return Outcome::No(e.to_string()),
    };
    let mut buf = [0u8; 4096];
    if let Err(e) = reader.read_at(&mut buf, 65536) {
        return Outcome::No(format!("read: {}", errno_name(&e)));
    }
    let events = rec.events_for(ino);
    let range = match events.first() {
        Some(Some((off, count))) => format!("read of 4096 @ 65536 reported range {off}+{count}"),
        Some(None) => "event raised without range info".to_owned(),
        None => return Outcome::No("no event for a read of a marked file".into()),
    };
    // One group serving placeholders in several room subvolumes.
    let other = scratch.join("pc-second-room");
    let second = btrfs::create_subvolume(&other)
        .and_then(|()| sparse_file(&other.join("pc"), 4096))
        .and_then(|f| rec.mark_inode(&f));
    let _ = btrfs::delete_subvolume(&other);
    match second {
        Ok(()) => Outcome::Yes(format!("{range}; one group marks inodes in two subvolumes")),
        Err(e) => Outcome::Yes(format!(
            "{range}; a second subvolume in the same group: {} (one group per room)",
            errno_name(&e)
        )),
    }
}

fn probe_snapshot(scratch: &Path) -> Outcome {
    let src = scratch.join("snapsrc");
    let dst = scratch.join("snapdst");
    let run = || -> io::Result<String> {
        btrfs::create_subvolume(&src)?;
        fs::write(src.join("f"), b"hello")?;
        let t = Instant::now();
        btrfs::snapshot(&src, &dst, true)?;
        let took = t.elapsed();
        let ro = fs::write(dst.join("g"), b"x").is_err();
        let same = fs::read(dst.join("f"))? == b"hello";
        btrfs::delete_subvolume(&dst)?;
        btrfs::delete_subvolume(&src)?;
        if !ro || !same {
            return Err(io::Error::other("snapshot is not a read-only copy"));
        }
        Ok(format!(
            "{:.2} ms for a small subvolume",
            took.as_secs_f64() * 1e3
        ))
    };
    match run() {
        Ok(d) => Outcome::Yes(d),
        Err(e) => {
            let _ = btrfs::delete_subvolume(&dst);
            let _ = btrfs::delete_subvolume(&src);
            Outcome::No(errno_name(&e))
        }
    }
}

fn probe_fiemap_diff(scratch: &Path) -> Outcome {
    let src = scratch.join("fiemap");
    let s1 = scratch.join("fiemap-1");
    let s2 = scratch.join("fiemap-2");
    let run = || -> io::Result<Outcome> {
        btrfs::create_subvolume(&src)?;
        let mut data = vec![0u8; 8 << 20];
        for (i, b) in data.iter_mut().enumerate() {
            *b = u8::try_from(i.wrapping_mul(31) % 251).unwrap_or(0);
        }
        let path = src.join("big");
        fs::write(&path, &data)?;
        btrfs::snapshot(&src, &s1, true)?;
        let f = OpenOptions::new().write(true).open(&path)?;
        f.write_all_at(&[0xEEu8; 4096], 3 << 20)?;
        drop(f);
        btrfs::snapshot(&src, &s2, true)?;
        let a = File::open(s1.join("big"))?;
        let b = File::open(s2.join("big"))?;
        let ranges = extents::diff_files(&a, &b)?;
        let changed: u64 = ranges.iter().map(|r| r.end.saturating_sub(r.start)).sum();
        let covers = ranges
            .iter()
            .any(|r| r.start <= 3 << 20 && r.end >= (3 << 20) + 4096);
        Ok(if covers && changed <= 256 * 1024 {
            Outcome::Yes(format!(
                "4 KiB write in 8 MiB → {changed} bytes flagged ({ranges:?})"
            ))
        } else {
            Outcome::No(format!("diff {ranges:?} does not isolate the write"))
        })
    };
    let out = run().unwrap_or_else(|e| Outcome::No(errno_name(&e)));
    for p in [&s2, &s1, &src] {
        let _ = btrfs::delete_subvolume(p);
    }
    out
}

fn probe_reflink(scratch: &Path) -> Outcome {
    let run = || -> io::Result<()> {
        let a = write_file(&scratch.join("rl-a"), 1 << 20, 7)?;
        let b = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(scratch.join("rl-b"))?;
        extents::reflink(&a, &b)?;
        let c = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(scratch.join("rl-c"))?;
        extents::reflink_range(&a, 0, 65536, &c, 0)?;
        let mut buf = [0u8; 16];
        c.read_at(&mut buf, 100)?;
        if buf != [7u8; 16] || b.metadata()?.len() != 1 << 20 {
            return Err(io::Error::other("clone content mismatch"));
        }
        Ok(())
    };
    match run() {
        Ok(()) => Outcome::Yes("whole-file and range clones share extents".into()),
        Err(e) => Outcome::No(errno_name(&e)),
    }
}

fn probe_k1(root: &Path, scratch: &Path) -> Outcome {
    // Files are written before the filesystem mark exists, so only the
    // reads below can raise events.
    let (Ok(resident), Ok(other)) = (
        write_file(&scratch.join("k1-resident"), 65536, 1),
        write_file(&scratch.join("k1-other"), 65536, 2),
    ) else {
        return Outcome::No("could not create test files".into());
    };
    let rec = match Recorder::start() {
        Ok(r) => r,
        Err(e) => return Outcome::No(format!("pre-content group: {}", errno_name(&e))),
    };
    if let Err(e) = rec.group.mark(
        fan::FAN_MARK_ADD | fan::FAN_MARK_FILESYSTEM,
        fan::FAN_PRE_ACCESS,
        None,
        Some(root),
    ) {
        return Outcome::No(format!(
            "filesystem FAN_PRE_ACCESS mark: {}",
            errno_name(&e)
        ));
    }
    let ignore = rec.group.mark_fd(
        fan::FAN_MARK_ADD | fan::FAN_MARK_IGNORE_SURV | fan::FAN_MARK_EVICTABLE,
        fan::FAN_PRE_ACCESS,
        resident.as_fd(),
    );
    if let Err(e) = ignore {
        return Outcome::No(format!("evictable ignore mark: {}", errno_name(&e)));
    }
    let mut buf = [0u8; 4096];
    let reads = (|| -> io::Result<()> {
        for _ in 0..10 {
            File::open(scratch.join("k1-resident"))?.read_at(&mut buf, 0)?;
        }
        File::open(scratch.join("k1-other"))?.read_at(&mut buf, 0)?;
        Ok(())
    })();
    if let Err(e) = reads {
        return Outcome::No(format!("read: {}", errno_name(&e)));
    }
    let ino = |f: &File| f.metadata().map_or(0, |m| m.ino());
    let suppressed = rec.events_for(ino(&resident)).len();
    let seen = rec.events_for(ino(&other)).len();
    if seen == 0 {
        Outcome::No("filesystem mark raised no event at all".into())
    } else if suppressed == 0 {
        Outcome::Yes(format!(
            "10 reads of the ignored inode: 0 events; unignored inode: {seen}"
        ))
    } else {
        Outcome::No(format!("ignored inode still raised {suppressed} events"))
    }
}

fn probe_k2(scratch: &Path) -> Outcome {
    let rec = match Recorder::start() {
        Ok(r) => r,
        Err(e) => return Outcome::No(format!("pre-content group: {}", errno_name(&e))),
    };
    let path = scratch.join("k2");
    let len: u64 = 4 << 20;
    let file = match sparse_file(&path, len) {
        Ok(f) => f,
        Err(e) => return Outcome::No(e.to_string()),
    };
    if let Err(e) = rec.mark_inode(&file) {
        return Outcome::No(format!("mark: {}", errno_name(&e)));
    }
    let ino = file.metadata().map_or(0, |m| m.ino());
    let Ok(reader) = File::open(&path) else {
        return Outcome::No("open failed".into());
    };
    let before_map = rec.events_for(ino).len();
    let map = match sys::Mmap::map(&reader, len) {
        Ok(m) => m,
        Err(e) => return Outcome::No(format!("mmap: {}", errno_name(&e))),
    };
    let at_map = rec.events_for(ino);
    let mut sum = 0u64;
    for page in (0..map.as_slice().len()).step_by(4096) {
        sum = sum.wrapping_add(u64::from(map.as_slice().get(page).copied().unwrap_or(0)));
    }
    drop(map);
    let after_faults = rec.events_for(ino).len();
    let mmap_events = at_map.len().saturating_sub(before_map);
    let fault_events = after_faults.saturating_sub(at_map.len());
    let range = at_map.last().copied().flatten();
    let detail = format!(
        "mmap(): {mmap_events} event(s) {range:?}; 1024 page faults: {fault_events} event(s) (sum {sum})"
    );
    let covers_mapping = range.is_some_and(|(o, c)| o == 0 && c >= len);
    // K2 asks for a fault-time hook. Without one, an event for the whole
    // mapping at mmap() time is just as safe: the daemon hydrates the
    // mapped range before the mapping exists.
    if (mmap_events > 0 && covers_mapping) || fault_events > 0 {
        Outcome::Yes(detail)
    } else {
        Outcome::No(detail)
    }
}

fn probe_k3(scratch: &Path) -> Outcome {
    let rec = match Recorder::start() {
        Ok(r) => r,
        Err(e) => return Outcome::No(format!("pre-content group: {}", errno_name(&e))),
    };
    let mut covered = Vec::new();
    let mut uncovered = Vec::new();
    for op in ["read", "splice", "sendfile", "copy_file_range", "FICLONE"] {
        let src_path = scratch.join(format!("k3-src-{op}"));
        let (Ok(src), Ok(dst)) = (
            sparse_file(&src_path, 1 << 20),
            sparse_file(&scratch.join(format!("k3-dst-{op}")), 0),
        ) else {
            return Outcome::No("could not create test files".into());
        };
        if let Err(e) = rec.mark_inode(&src) {
            return Outcome::No(format!("mark: {}", errno_name(&e)));
        }
        let ino = src.metadata().map_or(0, |m| m.ino());
        let Ok(reader) = File::open(&src_path) else {
            return Outcome::No("open failed".into());
        };
        let result = k3_op(op, &reader, &dst);
        let n = rec.events_for(ino).len();
        match (result, n) {
            (Err(e), _) => uncovered.push(format!("{op} failed: {}", errno_name(&e))),
            (Ok(()), 0) => uncovered.push(op.to_owned()),
            (Ok(()), _) => covered.push(op),
        }
    }
    let detail = format!("covered: {covered:?}; not covered: {uncovered:?}");
    if uncovered.is_empty() {
        Outcome::Yes(detail)
    } else {
        Outcome::No(detail)
    }
}

fn k3_op(op: &str, src: &File, dst: &File) -> io::Result<()> {
    let src_fd = src.as_raw_fd();
    let dst_fd = dst.as_raw_fd();
    let mut off: libc::loff_t = 0;
    match op {
        "read" => {
            let mut buf = [0u8; 4096];
            src.read_at(&mut buf, 0).map(|_| ())
        }
        "splice" => {
            let mut fds = [0 as libc::c_int; 2];
            // SAFETY: `fds` has room for two descriptors.
            sys::cvt(unsafe { libc::pipe(fds.as_mut_ptr()) })?;
            let [pipe_r, pipe_w] = fds;
            // SAFETY: valid descriptors and an in-memory offset.
            let n = unsafe {
                libc::splice(src_fd, &raw mut off, pipe_w, std::ptr::null_mut(), 4096, 0)
            };
            // SAFETY: closing the pipe created above.
            unsafe {
                libc::close(pipe_r);
                libc::close(pipe_w);
            }
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        "sendfile" => {
            // SAFETY: valid descriptors and an in-memory offset.
            let n = unsafe { libc::sendfile(dst_fd, src_fd, &raw mut off, 4096) };
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        "copy_file_range" => {
            let mut doff: libc::loff_t = 0;
            // SAFETY: valid descriptors and in-memory offsets.
            let n = unsafe {
                libc::copy_file_range(src_fd, &raw mut off, dst_fd, &raw mut doff, 4096, 0)
            };
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        _ => extents::reflink(src, dst),
    }
}

fn probe_k4(scratch: &Path) -> Outcome {
    let rec = match Recorder::start() {
        Ok(r) => r,
        Err(e) => return Outcome::No(format!("pre-content group: {}", errno_name(&e))),
    };
    let mut results = Vec::new();
    let mut all = true;
    for errno in [libc::EIO, libc::EBUSY, libc::EROFS, libc::EPERM] {
        let path = scratch.join(format!("k4-{errno}"));
        let Ok(file) = sparse_file(&path, 65536) else {
            return Outcome::No("could not create test file".into());
        };
        if let Err(e) = rec.mark_inode(&file) {
            return Outcome::No(format!("mark: {}", errno_name(&e)));
        }
        rec.response
            .store(fan::deny_errno(errno), Ordering::Relaxed);
        let mut buf = [0u8; 16];
        let got = File::open(&path).and_then(|f| f.read_at(&mut buf, 0));
        let want = errno_name(&io::Error::from_raw_os_error(errno));
        let observed = match got {
            Ok(_) => "read succeeded".to_owned(),
            Err(e) => errno_name(&e),
        };
        if observed != want {
            all = false;
        }
        results.push(format!("{want}→{observed}"));
    }
    rec.response.store(fan::FAN_ALLOW, Ordering::Relaxed);
    let rejected = rec
        .respond_errors
        .lock()
        .map(|e| e.clone())
        .unwrap_or_default();
    let detail = format!("{} (responses rejected: {rejected:?})", results.join(", "));
    if all {
        Outcome::Yes(detail)
    } else {
        Outcome::No(detail)
    }
}

/// Measures snapshot latency of a subvolume with `params.files` files while
/// `params.dirty_bytes` of unflushed writes are pending.
pub fn probe_k5(scratch: &Path, params: K5Params) -> io::Result<SnapshotCost> {
    let vol = scratch.join("k5");
    if !vol.exists() {
        btrfs::create_subvolume(&vol)?;
    }
    let per_dir = 1000usize;
    for i in 0..params.files {
        let dir = vol.join(format!("d{:04}", i / per_dir));
        if i % per_dir == 0 {
            fs::create_dir_all(&dir)?;
        }
        fs::write(
            dir.join(format!("f{i:06}")),
            [u8::try_from(i % 251).unwrap_or(0); 4096],
        )?;
    }
    let big = vol.join("dirty.bin");
    let chunk = vec![0x5Au8; 1 << 20];
    let mut cost = SnapshotCost {
        files: params.files,
        dirty_bytes: params.dirty_bytes,
        samples: Vec::new(),
    };
    for s in 0..params.samples {
        let f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&big)?;
        let mut written = 0u64;
        let mut at = 0u64;
        while written < params.dirty_bytes {
            f.write_all_at(&chunk, at)?;
            written = written.saturating_add(chunk.len() as u64);
            at = at.saturating_add(chunk.len() as u64);
        }
        drop(f);
        let snap = scratch.join(format!("k5-snap-{s}"));
        let t = Instant::now();
        btrfs::snapshot(&vol, &snap, true)?;
        cost.samples.push(t.elapsed());
        btrfs::delete_subvolume(&snap)?;
    }
    btrfs::delete_subvolume(&vol)?;
    Ok(cost)
}

fn probe_k6(root: &Path) -> Outcome {
    let group = match Group::new(fan::FAN_CLASS_NOTIF | fan::FAN_REPORT_DFID_NAME, 0) {
        Ok(g) => g,
        Err(e) => return Outcome::No(format!("fanotify_init: {}", errno_name(&e))),
    };
    match group.mark(
        fan::FAN_MARK_ADD | fan::FAN_MARK_MOUNT,
        fan::FAN_CREATE | fan::FAN_DELETE | fan::FAN_RENAME,
        None,
        Some(root),
    ) {
        Ok(()) => Outcome::Yes("mount mark accepted directory-entry events".into()),
        Err(e) => Outcome::No(format!(
            "mount mark with CREATE/DELETE/RENAME: {} — a dedicated superblock stays required",
            errno_name(&e)
        )),
    }
}

fn probe_fuse_passthrough() -> Outcome {
    if !Path::new("/dev/fuse").exists() {
        return Outcome::No("/dev/fuse missing".into());
    }
    // Passthrough can only be proven by mounting a FUSE filesystem, which is
    // the Tier 2 daemon's job; report the kernel's advertised limit.
    match fs::read_to_string("/sys/module/fuse/parameters/max_user_bgreq") {
        Ok(_) => {
            Outcome::Unknown("FUSE present; passthrough is verified by the FUSE daemon".into())
        }
        Err(_) => Outcome::Unknown("FUSE present (module parameters unreadable)".into()),
    }
}
