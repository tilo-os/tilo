//! What a job is, as data: portable, so a caller builds it the same way on
//! every platform and a backend decides how much of it it can enforce.

use std::ffi::OsString;
use std::path::PathBuf;

pub use sandbox_core::seccomp::CiProfile;

/// One job: a command, the filesystem it sees and how it is confined.
#[derive(Debug, Clone)]
pub struct JobSpec {
    /// The program and its arguments. A program without a `/` is looked up
    /// in the job's own `PATH` (from [`JobSpec::env`]), inside the sandbox.
    pub argv: Vec<OsString>,
    /// The job's whole environment; nothing is inherited from the caller.
    pub env: Vec<(OsString, OsString)>,
    /// The working directory, inside the sandbox.
    pub cwd: PathBuf,
    pub root: Root,
    /// Mounted in order of their destination's depth, so a mount inside
    /// another mount's destination lands on top of it.
    pub mounts: Vec<Mount>,
    /// The job's `/tmp`.
    pub tmp: Tmp,
    /// Host device nodes bound at the same path (`/dev/kvm`, `/dev/fuse`).
    /// `/dev/{null,zero,full,random,urandom,tty}` are always there.
    pub devices: Vec<PathBuf>,
    pub hostname: String,
    pub ids: IdMap,
    pub seccomp: Seccomp,
    pub landlock: Landlock,
    /// Mount the job's own cgroup, read-only, at `/sys/fs/cgroup` (on an
    /// otherwise empty `/sys`), so tools see its CPU and memory limits.
    pub cgroup_view: bool,
}

impl JobSpec {
    /// A job running `argv` in `root`, with the defaults every job gets: an
    /// empty environment but for a standard `PATH`, `/` as its working
    /// directory, a private tmpfs `/tmp`, the CI seccomp filter and
    /// Landlock required.
    pub fn new(root: Root, argv: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            env: vec![("PATH".into(), DEFAULT_PATH.into())],
            cwd: "/".into(),
            root,
            mounts: Vec::new(),
            tmp: Tmp::Tmpfs { size: None },
            devices: Vec::new(),
            hostname: "job".into(),
            ids: IdMap::current(),
            seccomp: Seccomp::Ci(CiProfile::Standard),
            landlock: Landlock::Required,
            cgroup_view: true,
        }
    }

    /// Set `key` in the job's environment, replacing an earlier value.
    pub fn env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> &mut Self {
        let key = key.into();
        self.env.retain(|(k, _)| *k != key);
        self.env.push((key, value.into()));
        self
    }

    /// Add a mount.
    pub fn mount(&mut self, mount: Mount) -> &mut Self {
        self.mounts.push(mount);
        self
    }

    /// Let the job start sandboxes of its own, as tests that run
    /// bubblewrap or `unshare` do: the nested seccomp profile
    /// ([`CiProfile::Nested`]), which allows new namespaces and mounts in
    /// them but still refuses netfilter and XFRM netlink, `bpf`, `io_uring`,
    /// the keyrings, userfaultfd and perf; and no Landlock, which refuses
    /// every mount to a process it confines, in any namespace. The job
    /// itself still holds no capability: what it mounts, it mounts in a
    /// user namespace it creates.
    ///
    /// A job that would be root inside becomes [`NESTING_ID`] instead. Its
    /// root holds no capability, and since Linux 5.12 mapping uid 0 of the
    /// job's namespace into a new one (`unshare --map-root-user`,
    /// bubblewrap's own namespace) takes `CAP_SETFCAP` there, so writing
    /// `uid_map` fails with `EPERM`. Any other id maps itself as an
    /// unprivileged user would. Set `ids` afterwards to choose another
    /// non-root id.
    pub fn allow_nested_sandboxes(&mut self) -> &mut Self {
        self.seccomp = Seccomp::Ci(CiProfile::Nested);
        self.landlock = Landlock::Off;
        if self.ids.inside_uid == 0 {
            self.ids.inside_uid = NESTING_ID;
        }
        if self.ids.inside_gid == 0 {
            self.ids.inside_gid = NESTING_ID;
        }
        self
    }
}

/// Who a job that starts sandboxes of its own is inside, unless its caller
/// names another non-root id (see [`JobSpec::allow_nested_sandboxes`]).
pub const NESTING_ID: u32 = 1000;

/// `PATH` when the job names none.
pub const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The job's root filesystem. Always read-only: what a job may write is
/// mounted writable on top.
#[derive(Debug, Clone)]
pub enum Root {
    /// An unpacked image. Every mount's destination must already exist in
    /// it: the job can't create directories in a read-only root, so
    /// [`crate::job::prepare_mount_points`] makes them beforehand.
    Image(PathBuf),
    /// The host's own `/usr` and `/etc` (read-only), the usual links or
    /// directories for `/bin`, `/sbin` and `/lib*`, and `extra` read-only
    /// paths at the same place, on an otherwise empty root.
    Host { extra: Vec<PathBuf> },
}

/// What is mounted at a destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// Inside the sandbox; absolute.
    pub dst: PathBuf,
    pub kind: MountKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountKind {
    /// A host path, read-only.
    ReadOnly(PathBuf),
    /// A host path the job may write: Landlock grants every right beneath it.
    ReadWrite(PathBuf),
    /// A tmpfs of at most `size` bytes (the job's cgroup pays for it).
    Tmpfs { size: Option<u64> },
    /// `lower`, read-only, with what the job writes going to `upper`
    /// (`work` is overlayfs's scratch, on the same filesystem as `upper`).
    Overlay {
        lower: PathBuf,
        upper: PathBuf,
        work: PathBuf,
    },
}

impl Mount {
    pub fn read_only(src: impl Into<PathBuf>, dst: impl Into<PathBuf>) -> Self {
        Self {
            dst: dst.into(),
            kind: MountKind::ReadOnly(src.into()),
        }
    }

    pub fn read_write(src: impl Into<PathBuf>, dst: impl Into<PathBuf>) -> Self {
        Self {
            dst: dst.into(),
            kind: MountKind::ReadWrite(src.into()),
        }
    }

    pub fn tmpfs(dst: impl Into<PathBuf>, size: Option<u64>) -> Self {
        Self {
            dst: dst.into(),
            kind: MountKind::Tmpfs { size },
        }
    }

    /// Whether the job may write beneath the destination.
    pub fn writable(&self) -> bool {
        !matches!(self.kind, MountKind::ReadOnly(_))
    }

    /// The host path the mount shows, if it shows one.
    pub fn source(&self) -> Option<&std::path::Path> {
        match &self.kind {
            MountKind::ReadOnly(p) | MountKind::ReadWrite(p) => Some(p),
            MountKind::Overlay { lower, .. } => Some(lower),
            MountKind::Tmpfs { .. } => None,
        }
    }
}

/// The job's `/tmp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tmp {
    /// A tmpfs; its pages count against the job's memory.
    Tmpfs { size: Option<u64> },
    /// A host directory: builds write gigabytes to `TMPDIR`, which belongs on
    /// disk, under the job's disk quota.
    Dir(PathBuf),
}

/// Who the job is, inside and outside its user namespace.
///
/// Without a [`IdBlock`], one uid and one gid are mapped: the job runs as
/// `outside_uid` on the host, and every other owner reads as `nobody`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdMap {
    pub inside_uid: u32,
    pub inside_gid: u32,
    /// The host uid the job's files are created as. Mapping any uid but
    /// the caller's own needs `CAP_SETUID` (a root worker).
    pub outside_uid: u32,
    pub outside_gid: u32,
    /// Host ids of the job's own, apart from the files' owner.
    pub block: Option<IdBlock>,
}

impl IdMap {
    /// Root inside, the caller's own uid and gid outside.
    pub fn current() -> Self {
        Self {
            inside_uid: 0,
            inside_gid: 0,
            outside_uid: rustix::process::geteuid().as_raw(),
            outside_gid: rustix::process::getegid().as_raw(),
            block: None,
        }
    }
}

/// A block of host ids a job runs as: ids `0..count` in its user namespace
/// are host uids `uid..uid + count` and gids `gid..gid + count`, and the job
/// runs as host uid `uid + inside_uid`.
///
/// The kernel keeps some limits per host user: pipe buffers
/// (`fs.pipe-user-pages-soft`), processes (`RLIMIT_NPROC`), inotify
/// instances and watches, keys, POSIX message queues. Jobs that all run as
/// the caller's uid share them, so one job's thousand pipes shrink every
/// other job's to two pages. A job with a block of its own has them to
/// itself.
///
/// Its files stay the caller's: every host path bound into the job is an
/// idmapped mount on which the files of `outside_uid`/`outside_gid` show as
/// the job's own (`inside_uid`/`inside_gid`) and what the job creates is
/// owned by `outside_uid`/`outside_gid` on the host, so the next job, with
/// another block, finds them as its own too. Other owners read as `nobody`,
/// as without a block. A writable path whose filesystem can't be idmapped
/// fails the job; a read-only one (or a device) is bound as it is, its
/// owners read as `nobody`.
///
/// Needs `CAP_SETUID`, `CAP_SETGID` and `CAP_SYS_ADMIN` in the initial user
/// namespace (a root worker) and Linux 5.12 (`mount_setattr` with
/// `MOUNT_ATTR_IDMAP`); overlays need 5.19, for idmapped layers. `count`
/// must exceed `inside_uid` and `inside_gid`. Blocks of jobs that run at
/// the same time must not overlap; the caller hands them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdBlock {
    pub uid: u32,
    pub gid: u32,
    pub count: u32,
}

/// The job's system-call filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seccomp {
    Ci(CiProfile),
    /// No filter. For tests of the sandbox itself; never for a job.
    Off,
}

/// Whether the job runs only under a fully enforced Landlock ruleset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landlock {
    /// Refuse to start on a kernel without Landlock.
    Required,
    /// Enforce what the kernel supports; run unconfined without Landlock.
    BestEffort,
    Off,
}
