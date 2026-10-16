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
    /// holds no capability over the namespaces the library made for it:
    /// what it mounts, it mounts in a user namespace of its own.
    ///
    /// With a range of subordinate ids ([`IdMap::range`]) the library makes
    /// that namespace: the command runs as root in a user and mount
    /// namespace beneath the job's, with every capability there and none
    /// over the job's mounts or network, as a container's root does under
    /// rootless podman. Image files then belong to root, files of many
    /// owners can be made (image builds), `setgroups` works, and the
    /// command gets a private `XDG_RUNTIME_DIR`. Without a range it runs
    /// capless as the job's one mapped id.
    pub fn allow_nested_sandboxes(&mut self) -> &mut Self {
        self.seccomp = Seccomp::Ci(CiProfile::Nested);
        self.landlock = Landlock::Off;
        self
    }
}

impl JobSpec {
    /// Whether the command runs in a user namespace of its own, as root
    /// over the job's whole id range (see [`JobSpec::allow_nested_sandboxes`]).
    pub(crate) fn own_user_namespace(&self) -> bool {
        self.seccomp == Seccomp::Ci(CiProfile::Nested) && self.ids.range.is_some()
    }

    /// The uid and gid the command runs as, inside.
    pub(crate) fn command_ids(&self) -> (u32, u32) {
        if self.own_user_namespace() {
            (0, 0)
        } else {
            (self.ids.inside_uid, self.ids.inside_gid)
        }
    }

    /// The private `XDG_RUNTIME_DIR` a job that may start sandboxes of its
    /// own gets, as desktop sessions and bus daemons in tests expect.
    pub(crate) fn runtime_dir(&self) -> Option<PathBuf> {
        (self.seccomp == Seccomp::Ci(CiProfile::Nested))
            .then(|| PathBuf::from(format!("/run/user/{}", self.command_ids().0)))
    }
}

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

/// Who the job is, inside and outside its user namespace. Without a
/// [`range`](IdMap::range), one uid and one gid are mapped and every other
/// owner reads as `nobody`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdMap {
    pub inside_uid: u32,
    pub inside_gid: u32,
    /// The host uid the job's files are created as. Mapping any uid but
    /// the caller's own needs `CAP_SETUID` (a root worker).
    pub outside_uid: u32,
    pub outside_gid: u32,
    /// Subordinate ids, mapped as uids and gids `1..=count` above root
    /// (`inside_uid` and `inside_gid` must then be 0). Only a job that may
    /// start sandboxes of its own ([`JobSpec::allow_nested_sandboxes`])
    /// maps them; every other job keeps its one id.
    pub range: Option<IdRange>,
}

impl IdMap {
    /// Root inside, the caller's own uid and gid outside.
    pub fn current() -> Self {
        Self {
            inside_uid: 0,
            inside_gid: 0,
            outside_uid: rustix::process::geteuid().as_raw(),
            outside_gid: rustix::process::getegid().as_raw(),
            range: None,
        }
    }
}

/// Host ids a job may own besides the caller's: the caller's entries in
/// `/etc/subuid` and `/etc/subgid`, or a range a root caller owns outright.
/// The library writes the maps itself where the caller holds `CAP_SETUID`
/// and `CAP_SETGID`, and asks `newuidmap` and `newgidmap` otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdRange {
    /// The first host uid.
    pub uid: u32,
    /// The first host gid.
    pub gid: u32,
    /// How many ids, from each first id.
    pub count: u32,
}

impl IdRange {
    /// The ids a container's root expects to hand out: 65536.
    pub const FULL: u32 = 65536;

    /// The caller's subordinate ids: its first entry in `/etc/subuid` and
    /// in `/etc/subgid` (by name or uid), at most [`Self::FULL`] of them.
    /// `None` without an entry in both.
    pub fn subordinate() -> Option<Self> {
        let uid = rustix::process::geteuid().as_raw();
        let name = user_name(uid);
        let find = |file: &str| {
            let text = std::fs::read_to_string(file).ok()?;
            parse_subordinate(&text, uid, name.as_deref())
        };
        let (uid_start, uid_count) = find("/etc/subuid")?;
        let (gid_start, gid_count) = find("/etc/subgid")?;
        Some(Self {
            uid: uid_start,
            gid: gid_start,
            count: uid_count.min(gid_count).min(Self::FULL),
        })
        .filter(|r| r.count > 0)
    }
}

/// The first `start:count` in `/etc/subuid`-style `text` for this user.
fn parse_subordinate(text: &str, uid: u32, name: Option<&str>) -> Option<(u32, u32)> {
    let uid = uid.to_string();
    text.lines().find_map(|line| {
        let mut f = line.trim().split(':');
        let (who, start, count) = (f.next()?, f.next()?, f.next()?);
        if who != uid && Some(who) != name {
            return None;
        }
        Some((start.parse().ok()?, count.parse().ok()?))
    })
}

/// The name `/etc/passwd` gives `uid`.
fn user_name(uid: u32) -> Option<String> {
    let text = std::fs::read_to_string("/etc/passwd").ok()?;
    let uid = uid.to_string();
    text.lines().find_map(|l| {
        let mut f = l.split(':');
        let name = f.next()?;
        (f.nth(1)? == uid).then(|| name.to_owned())
    })
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

#[cfg(test)]
mod tests {
    use super::parse_subordinate;

    #[test]
    fn subordinate_ids_are_found_by_name_or_uid() {
        let text = "other:100000:65536\nci:524288:65536\n1000:700000:1000\n";
        assert_eq!(
            parse_subordinate(text, 1000, Some("ci")),
            Some((524_288, 65_536))
        );
        assert_eq!(parse_subordinate(text, 1000, None), Some((700_000, 1000)));
        assert_eq!(parse_subordinate(text, 1001, Some("nobody")), None);
        assert_eq!(parse_subordinate("ci:x:1\n", 7, Some("ci")), None);
    }
}
