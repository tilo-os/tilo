//! Everything the job's first process needs, prepared before `clone3`.
//!
//! The child is a copy of a multithreaded process: another thread may have
//! held the allocator's lock at the moment of the copy, so the child must
//! never allocate. This module turns a [`JobSpec`] into C strings, pointer
//! arrays, mount operations, Landlock rules and BPF programs, so the child
//! only reads them and makes system calls.

use std::ffi::{CString, OsStr};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use super::sys;
use crate::job::SpawnError;
use crate::job::spec::{JobSpec, Landlock, MountKind, Root, Seccomp, Tmp};

/// Where the old root is reachable while the new one is built.
const OLD: &str = "/oldroot";
/// Where the new root is built.
const NEW: &str = "/newroot";

/// One step of building the job's filesystem.
#[derive(Debug)]
pub enum Op {
    /// `mkdir`; failure is ignored (the mount that needs it reports).
    Mkdir(CString),
    /// Create an empty file to bind a file over; failure is ignored.
    Touch(CString),
    Symlink {
        target: CString,
        path: CString,
    },
    Mount {
        src: Option<CString>,
        dst: CString,
        fstype: Option<CString>,
        flags: libc::c_ulong,
        data: Option<CString>,
    },
    SetAttr {
        path: CString,
        set: u64,
        recursive: bool,
    },
    /// Attach an idmapped tree the worker made ([`Plan::trees`], by index).
    Attach {
        tree: usize,
        dst: CString,
    },
}

impl Op {
    /// Whether a failure of this step stops the job.
    pub fn fatal(&self) -> bool {
        matches!(
            self,
            Op::Mount { .. } | Op::SetAttr { .. } | Op::Symlink { .. } | Op::Attach { .. }
        )
    }

    /// The step in words, for an error: the path is the one inside the job.
    pub fn describe(&self) -> String {
        let inside = |p: &CString| {
            let s = p.to_string_lossy();
            match s.strip_prefix(NEW) {
                Some("") => "/".to_owned(),
                Some(rest) => rest.to_owned(),
                None => s.into_owned(),
            }
        };
        match self {
            Op::Mkdir(p) | Op::Touch(p) => format!("creating {}", inside(p)),
            Op::Symlink { path, .. } => format!("linking {}", inside(path)),
            Op::Mount { dst, fstype, .. } => match fstype.as_ref().map(|f| f.to_string_lossy()) {
                Some(f) => format!("mounting {} ({f})", inside(dst)),
                None => format!("binding {}", inside(dst)),
            },
            Op::SetAttr { path, .. } => format!("making {} read-only", inside(path)),
            Op::Attach { dst, .. } => format!("binding {} (idmapped)", inside(dst)),
        }
    }
}

/// A Landlock rule: a path inside the new root and the rights beneath it.
#[derive(Debug)]
pub struct Rule {
    pub path: CString,
    pub access: u64,
}

/// What the child does, all of it prepared.
#[derive(Debug)]
pub struct Plan {
    /// The directory the staging tmpfs is mounted on (an existing one).
    pub stage: CString,
    pub stage_old: CString,
    pub stage_new: CString,
    pub ops: Vec<Op>,
    /// Detached idmapped mounts for [`Op::Attach`], made by the worker.
    pub trees: Vec<OwnedFd>,
    /// The job's ids in its own namespace, which the init takes on before
    /// it builds the root: with a block of host ids, the init starts as
    /// the worker's uid, which the job's namespace doesn't map.
    pub become_ids: Option<(u32, u32)>,
    pub new_root: CString,
    pub hostname: CString,
    pub cwd: CString,
    /// Full paths to try `execve` on, in order.
    pub candidates: Vec<CString>,
    pub argv: Vec<CString>,
    pub envp: Vec<CString>,
    pub landlock: Option<(OwnedFd, Vec<Rule>)>,
    /// Installed in order.
    pub seccomp: Vec<Vec<libc::sock_filter>>,
    pub cap_last: u32,
    /// Where the caller's environment and arguments sit in its memory
    /// (`/proc/self/stat` fields 48–51), to be blanked in the job's init.
    pub env_area: (usize, usize),
    pub arg_area: (usize, usize),
}

fn cstr(bytes: &[u8], what: &str) -> Result<CString, SpawnError> {
    CString::new(bytes).map_err(|_| SpawnError::Invalid(format!("{what} contains a NUL byte")))
}

fn path_c(p: &Path) -> Result<CString, SpawnError> {
    cstr(p.as_os_str().as_bytes(), &p.display().to_string())
}

/// `prefix` joined with an absolute `inside` path.
fn under(prefix: &str, inside: &Path) -> Result<CString, SpawnError> {
    let mut bytes = prefix.as_bytes().to_vec();
    let rel = inside.as_os_str().as_bytes();
    if rel != b"/" {
        bytes.extend_from_slice(rel);
    }
    cstr(&bytes, &inside.display().to_string())
}

/// An absolute path without `..`, `.` or repeated separators, or an error
/// naming `what`.
fn normal(p: &Path, what: &str) -> Result<PathBuf, SpawnError> {
    if !p.is_absolute() {
        return Err(SpawnError::Invalid(format!(
            "{what} {} is not absolute",
            p.display()
        )));
    }
    let mut out = PathBuf::from("/");
    for c in p.components() {
        match c {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(n) => out.push(n),
            Component::ParentDir | Component::Prefix(_) => {
                return Err(SpawnError::Invalid(format!(
                    "{what} {} contains `..`",
                    p.display()
                )));
            }
        }
    }
    Ok(out)
}

/// The host path a source names, resolved (symlinks followed).
fn source(p: &Path) -> Result<(PathBuf, std::fs::Metadata), SpawnError> {
    let real = std::fs::canonicalize(p)
        .map_err(|e| SpawnError::Invalid(format!("{}: {e}", p.display())))?;
    let meta = std::fs::metadata(&real)
        .map_err(|e| SpawnError::Invalid(format!("{}: {e}", p.display())))?;
    Ok((real, meta))
}

/// Whether a bind must be idmapped for the job to use it: a writable one
/// must (the job could not write the owner's files otherwise); a read-only
/// one is bound as it is when its filesystem can't be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Idmap {
    Required,
    IfPossible,
}

struct Builder {
    ops: Vec<Op>,
    /// Paths made so far, so ancestors aren't made twice.
    made: Vec<PathBuf>,
    /// Whether the root is ours to create directories in.
    root_writable: bool,
    /// Landlock rules, in order; `/` (read) first.
    rules: Vec<Rule>,
    /// With a block of host ids: the user namespace binds are idmapped
    /// with, and the trees made so far.
    userns: Option<OwnedFd>,
    trees: Vec<OwnedFd>,
    /// Overlays mounted so far, for their layers' staging directories.
    overlays: usize,
}

impl Builder {
    fn mkdirs(&mut self, inside: &Path) -> Result<(), SpawnError> {
        let mut at = PathBuf::from("/");
        for c in inside.components().skip(1) {
            at.push(c);
            if !self.made.contains(&at) {
                self.made.push(at.clone());
                self.ops.push(Op::Mkdir(under(NEW, &at)?));
            }
        }
        Ok(())
    }

    /// A mount point at `inside`: a directory, or an empty file for a file
    /// source. In an image root they must exist already; trying costs a
    /// failed `mkdir`.
    fn point(&mut self, inside: &Path, file: bool) -> Result<(), SpawnError> {
        if let Some(parent) = inside.parent() {
            self.mkdirs(parent)?;
        }
        if file {
            self.ops.push(Op::Touch(under(NEW, inside)?));
        } else {
            self.mkdirs(inside)?;
        }
        Ok(())
    }

    fn bind(
        &mut self,
        real: &Path,
        inside: &Path,
        file: bool,
        idmap: Idmap,
    ) -> Result<(), SpawnError> {
        self.point(inside, file)?;
        let dst = under(NEW, inside)?;
        self.bind_at(real, dst, libc::MS_BIND | libc::MS_REC, idmap)
    }

    /// Bind the host path `real` at `dst` (a path in the staging root):
    /// idmapped where the job has host ids of its own, plainly otherwise.
    fn bind_at(
        &mut self,
        real: &Path,
        dst: CString,
        flags: libc::c_ulong,
        idmap: Idmap,
    ) -> Result<(), SpawnError> {
        if let Some(userns) = &self.userns {
            match super::idmap::tree(real, userns.as_fd()) {
                Ok(tree) => {
                    self.ops.push(Op::Attach {
                        tree: self.trees.len(),
                        dst,
                    });
                    self.trees.push(tree);
                    return Ok(());
                }
                Err(e) if idmap == Idmap::Required => {
                    return Err(SpawnError::Setup(format!(
                        "idmapping {} for the job's own ids: {e}{}",
                        real.display(),
                        if e.raw_os_error() == Some(libc::EINVAL) {
                            " (its filesystem can't be idmapped)"
                        } else {
                            ""
                        }
                    )));
                }
                Err(_) => {}
            }
        }
        self.ops.push(Op::Mount {
            src: Some(under(OLD, real)?),
            dst,
            fstype: None,
            flags,
            data: None,
        });
        Ok(())
    }

    fn attr(&mut self, inside: &Path, set: u64, recursive: bool) -> Result<(), SpawnError> {
        self.ops.push(Op::SetAttr {
            path: under(NEW, inside)?,
            set,
            recursive,
        });
        Ok(())
    }

    fn fs(
        &mut self,
        fstype: &str,
        inside: &Path,
        flags: libc::c_ulong,
        data: Option<&str>,
    ) -> Result<(), SpawnError> {
        self.point(inside, false)?;
        self.ops.push(Op::Mount {
            src: Some(cstr(fstype.as_bytes(), fstype)?),
            dst: under(NEW, inside)?,
            fstype: Some(cstr(fstype.as_bytes(), fstype)?),
            flags,
            data: data
                .map(|d| cstr(d.as_bytes(), "mount options"))
                .transpose()?,
        });
        Ok(())
    }

    /// Grant Landlock rights beneath a writable path.
    fn writable(&mut self, inside: &Path, dir: bool) -> Result<(), SpawnError> {
        self.rules.push(Rule {
            path: path_c(inside)?,
            access: if dir { u64::MAX } else { sys::LL_FILE_RIGHTS },
        });
        Ok(())
    }

    /// The job's root: an image bound read-only, or the host's /usr and /etc
    /// on a tmpfs.
    fn root(&mut self, root: &Root) -> Result<(), SpawnError> {
        let new = Path::new("/");
        match root {
            Root::Image(dir) => {
                let (src, meta) = source(dir)?;
                if !meta.is_dir() {
                    return Err(SpawnError::Invalid(format!(
                        "the root {} is not a directory",
                        dir.display()
                    )));
                }
                self.bind_at(
                    &src,
                    under(NEW, new)?,
                    libc::MS_BIND | libc::MS_REC,
                    Idmap::IfPossible,
                )?;
                // Read-only before anything is mounted on it: the job never
                // writes into a shared image, not even a mount point.
                self.attr(new, RO, false)?;
            }
            Root::Host { extra } => {
                self.ops.push(Op::Mount {
                    src: Some(cstr(b"tmpfs", "tmpfs")?),
                    dst: under(NEW, new)?,
                    fstype: Some(cstr(b"tmpfs", "tmpfs")?),
                    flags: libc::MS_NOSUID | libc::MS_NODEV,
                    data: Some(cstr(b"mode=0755,size=1m", "tmpfs options")?),
                });
                for top in ["/usr", "/etc"] {
                    let (src, _) = source(Path::new(top))?;
                    self.bind(&src, Path::new(top), false, Idmap::IfPossible)?;
                    self.attr(Path::new(top), RO, true)?;
                }
                for name in HOST_LINKS {
                    self.host_link(name)?;
                }
                for p in extra {
                    let inside = normal(p, "extra root path")?;
                    let (src, meta) = source(p)?;
                    self.bind(&src, &inside, !meta.is_dir(), Idmap::IfPossible)?;
                    self.attr(&inside, RO, true)?;
                }
            }
        }
        Ok(())
    }

    /// Mirror a top-level host entry (`/bin`, `/lib`, …): a symlink as a
    /// symlink, a directory as a read-only bind.
    fn host_link(&mut self, name: &str) -> Result<(), SpawnError> {
        let host = Path::new("/").join(name);
        match std::fs::symlink_metadata(&host) {
            Ok(m) if m.file_type().is_symlink() => {
                let target = std::fs::read_link(&host)
                    .map_err(|e| SpawnError::Invalid(format!("{}: {e}", host.display())))?;
                self.ops.push(Op::Symlink {
                    target: path_c(&target)?,
                    path: under(NEW, &host)?,
                });
            }
            Ok(m) if m.is_dir() => {
                let (src, _) = source(&host)?;
                self.bind(&src, &host, false, Idmap::IfPossible)?;
                self.attr(&host, RO, true)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// `/proc` and a minimal `/dev` with its device nodes and links.
    fn proc_and_dev(&mut self, spec: &JobSpec) -> Result<(), SpawnError> {
        self.fs(
            "proc",
            Path::new("/proc"),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            None,
        )?;
        let dev = Path::new("/dev");
        self.fs(
            "tmpfs",
            dev,
            libc::MS_NOSUID | libc::MS_NOEXEC,
            Some("mode=0755,size=1m"),
        )?;
        let devices = DEVICES
            .iter()
            .map(|d| dev.join(d))
            .chain(spec.devices.iter().cloned());
        for d in devices {
            let inside = normal(&d, "device")?;
            if std::fs::metadata(&d).is_err() {
                continue;
            }
            self.point(&inside, true)?;
            // Idmapped where the device's filesystem allows (a container's
            // tmpfs /dev does, devtmpfs doesn't): its owner's access then
            // stays the job's, as for a device of mode 0660.
            let dst = under(NEW, &inside)?;
            self.bind_at(&d, dst, libc::MS_BIND, Idmap::IfPossible)?;
        }
        for (link, target) in [
            ("fd", "/proc/self/fd"),
            ("stdin", "/proc/self/fd/0"),
            ("stdout", "/proc/self/fd/1"),
            ("stderr", "/proc/self/fd/2"),
            ("ptmx", "pts/ptmx"),
        ] {
            self.ops.push(Op::Symlink {
                target: cstr(target.as_bytes(), target)?,
                path: under(NEW, &dev.join(link))?,
            });
        }
        self.fs(
            "devpts",
            &dev.join("pts"),
            libc::MS_NOSUID | libc::MS_NOEXEC,
            Some("newinstance,ptmxmode=0666,mode=620"),
        )?;
        self.fs(
            "tmpfs",
            &dev.join("shm"),
            libc::MS_NOSUID | libc::MS_NODEV,
            Some("mode=1777"),
        )?;
        self.writable(dev, true)
    }

    /// `/tmp`, and the read-only cgroup view at `/sys/fs/cgroup`.
    fn tmp_and_sys(&mut self, spec: &JobSpec) -> Result<(), SpawnError> {
        let tmp = Path::new("/tmp");
        match &spec.tmp {
            Tmp::Tmpfs { size } => self.fs(
                "tmpfs",
                tmp,
                libc::MS_NOSUID | libc::MS_NODEV,
                Some(&tmpfs_options("1777", *size)),
            )?,
            Tmp::Dir(dir) => {
                let (src, _) = source(dir)?;
                self.bind(&src, tmp, false, Idmap::Required)?;
                self.attr(tmp, RW, true)?;
            }
        }
        self.writable(tmp, true)?;
        if spec.cgroup_view {
            let sys_dir = Path::new("/sys");
            self.fs(
                "tmpfs",
                sys_dir,
                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                Some("mode=0755,size=64k"),
            )?;
            self.fs(
                "cgroup2",
                Path::new("/sys/fs/cgroup"),
                libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC | libc::MS_RDONLY,
                None,
            )?;
            self.attr(sys_dir, sys::MOUNT_ATTR_RDONLY, false)?;
        }
        Ok(())
    }

    /// One caller mount, deepest-last so a nested one lands on top.
    fn caller_mount(&mut self, m: &crate::job::spec::Mount) -> Result<(), SpawnError> {
        let inside = normal(&m.dst, "mount destination")?;
        if inside == Path::new("/") {
            return Err(SpawnError::Invalid("a mount over / ; use Root".into()));
        }
        match &m.kind {
            MountKind::ReadOnly(p) => {
                let (src, meta) = source(p)?;
                self.bind(&src, &inside, !meta.is_dir(), Idmap::IfPossible)?;
                self.attr(&inside, RO, true)?;
            }
            MountKind::ReadWrite(p) => {
                let (src, meta) = source(p)?;
                self.bind(&src, &inside, !meta.is_dir(), Idmap::Required)?;
                self.attr(&inside, RW, true)?;
                self.writable(&inside, meta.is_dir())?;
            }
            MountKind::Tmpfs { size } => {
                self.fs(
                    "tmpfs",
                    &inside,
                    libc::MS_NOSUID | libc::MS_NODEV,
                    Some(&tmpfs_options("0755", *size)),
                )?;
                self.writable(&inside, true)?;
            }
            MountKind::Overlay { lower, upper, work } => {
                let opts = if self.userns.is_some() {
                    self.idmapped_layers(lower, upper, work)?
                } else {
                    overlay_options(lower, upper, work)?
                };
                self.fs(
                    "overlay",
                    &inside,
                    libc::MS_NOSUID | libc::MS_NODEV,
                    Some(&opts),
                )?;
                self.writable(&inside, true)?;
            }
        }
        Ok(())
    }
}

impl Builder {
    /// An overlay's layers as idmapped mounts in the staging root, and its
    /// `-o` options naming them: overlayfs creates what the job writes with
    /// the job's ids, which only an idmapped upper layer stores as the
    /// owner's. The upper and work directories must share a mount, so their
    /// common parent is the one idmapped.
    fn idmapped_layers(
        &mut self,
        lower: &Path,
        upper: &Path,
        work: &Path,
    ) -> Result<String, SpawnError> {
        use std::fmt::Write as _;
        let real = |p: &Path| {
            std::fs::canonicalize(p)
                .map_err(|e| SpawnError::Invalid(format!("{}: {e}", p.display())))
        };
        let (lower, upper, work) = (real(lower)?, real(upper)?, real(work)?);
        let mut common = upper.clone();
        while !work.starts_with(&common) {
            if !common.pop() {
                break;
            }
        }
        let beneath = |p: &Path| {
            p.strip_prefix(&common)
                .map(Path::to_path_buf)
                .unwrap_or_default()
        };
        let at = PathBuf::from(format!("/layers/{}", self.overlays));
        self.overlays = self.overlays.saturating_add(1);
        let (lower_at, upper_at) = (at.join("lower"), at.join("upper"));
        for dir in [Path::new("/layers"), &at, &lower_at, &upper_at] {
            self.ops.push(Op::Mkdir(path_c(dir)?));
        }
        self.bind_at(
            &lower,
            path_c(&lower_at)?,
            libc::MS_BIND | libc::MS_REC,
            Idmap::Required,
        )?;
        self.bind_at(
            &common,
            path_c(&upper_at)?,
            libc::MS_BIND | libc::MS_REC,
            Idmap::Required,
        )?;
        let mut opts = String::from("userxattr");
        for (key, p) in [
            ("lowerdir", lower_at),
            ("upperdir", upper_at.join(beneath(&upper))),
            ("workdir", upper_at.join(beneath(&work))),
        ] {
            let shown = p.display().to_string();
            if shown.contains([',', ':', '\\']) {
                return Err(SpawnError::Invalid(format!(
                    "overlay path {shown} contains a comma, colon or backslash"
                )));
            }
            let _ = write!(opts, ",{key}={shown}");
        }
        Ok(opts)
    }
}

/// overlayfs `-o` options, with the old root's prefix on each path.
fn overlay_options(lower: &Path, upper: &Path, work: &Path) -> Result<String, SpawnError> {
    use std::fmt::Write as _;
    let mut opts = String::from("userxattr");
    for (key, p) in [("lowerdir", lower), ("upperdir", upper), ("workdir", work)] {
        let real = std::fs::canonicalize(p)
            .map_err(|e| SpawnError::Invalid(format!("{}: {e}", p.display())))?;
        let shown = format!("{OLD}{}", real.display());
        if shown.contains([',', ':', '\\']) {
            return Err(SpawnError::Invalid(format!(
                "overlay path {} contains a comma, colon or backslash",
                p.display()
            )));
        }
        let _ = write!(opts, ",{key}={shown}");
    }
    Ok(opts)
}

const RO: u64 = sys::MOUNT_ATTR_RDONLY | sys::MOUNT_ATTR_NOSUID | sys::MOUNT_ATTR_NODEV;
const RW: u64 = sys::MOUNT_ATTR_NOSUID | sys::MOUNT_ATTR_NODEV;

/// Device nodes every job gets, bound from the host.
const DEVICES: [&str; 6] = ["null", "zero", "full", "random", "urandom", "tty"];

/// The top-level host entries a host root mirrors besides `/usr` and `/etc`.
const HOST_LINKS: [&str; 6] = ["bin", "sbin", "lib", "lib32", "lib64", "libx32"];

fn tmpfs_options(mode: &str, size: Option<u64>) -> String {
    match size {
        Some(s) => format!("mode={mode},size={s}"),
        None => format!("mode={mode}"),
    }
}

/// An existing directory to mount the staging tmpfs on. It is covered only
/// inside the job's mount namespace.
fn stage_dir() -> &'static str {
    ["/tmp", "/var/tmp", "/mnt", "/run"]
        .into_iter()
        .find(|d| Path::new(d).is_dir())
        .unwrap_or("/tmp")
}

/// What [`filesystem`] makes: the operations, the Landlock rules and the
/// idmapped trees the operations attach.
type Filesystem = (Vec<Op>, Vec<Rule>, Vec<OwnedFd>);

fn filesystem(spec: &JobSpec) -> Result<Filesystem, SpawnError> {
    let userns = match spec.ids.block {
        Some(_) => Some(super::idmap::namespace(&spec.ids).map_err(|e| {
            SpawnError::Setup(format!(
                "a user namespace mapping the job's files to its own ids: {e}"
            ))
        })?),
        None => None,
    };
    let mut b = Builder {
        ops: Vec::new(),
        made: Vec::new(),
        root_writable: matches!(spec.root, Root::Host { .. }),
        rules: vec![Rule {
            path: path_c(Path::new("/"))?,
            access: sys::LL_READ,
        }],
        userns,
        trees: Vec::new(),
        overlays: 0,
    };
    b.root(&spec.root)?;
    b.proc_and_dev(spec)?;
    b.tmp_and_sys(spec)?;
    let mut mounts: Vec<_> = spec.mounts.iter().collect();
    mounts.sort_by_key(|m| m.dst.components().count());
    for m in mounts {
        b.caller_mount(m)?;
    }
    if b.root_writable {
        b.attr(Path::new("/"), sys::MOUNT_ATTR_RDONLY, false)?;
    }
    Ok((b.ops, b.rules, b.trees))
}

fn candidates(spec: &JobSpec) -> Result<Vec<CString>, SpawnError> {
    let program = spec
        .argv
        .first()
        .ok_or_else(|| SpawnError::Invalid("an empty command".into()))?
        .as_bytes();
    if program.is_empty() {
        return Err(SpawnError::Invalid("an empty program name".into()));
    }
    if program.contains(&b'/') {
        return Ok(vec![cstr(program, "the program")?]);
    }
    let path = spec
        .env
        .iter()
        .rev()
        .find(|(k, _)| k == OsStr::new("PATH"))
        .map_or(crate::job::spec::DEFAULT_PATH.as_bytes(), |(_, v)| {
            v.as_bytes()
        });
    path.split(|b| *b == b':')
        .filter(|d| !d.is_empty())
        .map(|d| {
            let mut full = d.to_vec();
            full.push(b'/');
            full.extend_from_slice(program);
            cstr(&full, "the program")
        })
        .collect()
}

/// Where this process's environment and arguments are in its memory.
fn areas() -> ((usize, usize), (usize, usize)) {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // The command name (field 2) may contain spaces; fields after it don't.
    let after = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
    let fields: Vec<usize> = after
        .split_whitespace()
        .map(|f| f.parse().unwrap_or(0))
        .collect();
    // Fields 48–51 of the whole line are arg_start, arg_end, env_start,
    // env_end: indices 45–48 after the command name (field 3 is index 0).
    let get = |i: usize| fields.get(i).copied().unwrap_or(0);
    ((get(45), get(46)), (get(47), get(48)))
}

fn seccomp_filters(mode: Seccomp) -> Result<Vec<Vec<libc::sock_filter>>, SpawnError> {
    use sandbox_core::seccomp;
    let Seccomp::Ci(profile) = mode else {
        return Ok(Vec::new());
    };
    let compile = |e: seccomp::SeccompError| SpawnError::Invalid(format!("seccomp: {e}"));
    let programs = [
        seccomp::clone3_compat_program().map_err(compile)?,
        seccomp::ci_program(profile).map_err(compile)?,
    ];
    Ok(programs
        .iter()
        .map(|p| {
            seccomp::program_bytes(p)
                .chunks_exact(8)
                .map(|c| {
                    let word = |a: usize, b: usize| c.get(a..b).unwrap_or_default();
                    libc::sock_filter {
                        code: u16::from_ne_bytes(word(0, 2).try_into().unwrap_or([0; 2])),
                        jt: c.get(2).copied().unwrap_or(0),
                        jf: c.get(3).copied().unwrap_or(0),
                        k: u32::from_ne_bytes(word(4, 8).try_into().unwrap_or([0; 4])),
                    }
                })
                .collect()
        })
        .collect())
}

fn landlock(
    mode: Landlock,
    mut rules: Vec<Rule>,
) -> Result<Option<(OwnedFd, Vec<Rule>)>, SpawnError> {
    if mode == Landlock::Off {
        return Ok(None);
    }
    let Some(abi) = sys::landlock_abi() else {
        return match mode {
            Landlock::Required => Err(SpawnError::Unsupported(
                "this kernel has no Landlock (it needs CONFIG_SECURITY_LANDLOCK and `landlock` in lsm=)".into(),
            )),
            _ => Ok(None),
        };
    };
    let handled = sys::landlock_fs_rights(abi);
    for r in &mut rules {
        r.access &= handled;
    }
    let fd = sys::landlock_ruleset(handled)
        .map_err(|e| SpawnError::Unsupported(format!("creating a Landlock ruleset: {e}")))?;
    Ok(Some((fd, rules)))
}

/// Landlock as the job runs it. A job that may start sandboxes of its own
/// mounts in namespaces it creates, which Landlock refuses to every process
/// it confines: it runs without.
fn landlock_mode(spec: &JobSpec) -> Landlock {
    if spec.seccomp == Seccomp::Ci(crate::job::spec::CiProfile::Nested) {
        Landlock::Off
    } else {
        spec.landlock
    }
}

impl Plan {
    pub fn new(spec: &JobSpec) -> Result<Self, SpawnError> {
        let cwd = normal(&spec.cwd, "working directory")?;
        let become_ids = match spec.ids.block {
            Some(block) => {
                if spec.ids.inside_uid >= block.count || spec.ids.inside_gid >= block.count {
                    return Err(SpawnError::Invalid(format!(
                        "the job's ids {}:{} are outside its block of {}",
                        spec.ids.inside_uid, spec.ids.inside_gid, block.count
                    )));
                }
                Some((spec.ids.inside_uid, spec.ids.inside_gid))
            }
            None => None,
        };
        let (ops, rules, trees) = filesystem(spec)?;
        let stage = stage_dir();
        let (arg_area, env_area) = areas();
        let cap_last = std::fs::read_to_string("/proc/sys/kernel/cap_last_cap")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(63);
        Ok(Self {
            stage: cstr(stage.as_bytes(), stage)?,
            stage_old: cstr(OLD.trim_start_matches('/').as_bytes(), OLD)?,
            stage_new: cstr(NEW.trim_start_matches('/').as_bytes(), NEW)?,
            ops,
            trees,
            become_ids,
            new_root: cstr(NEW.as_bytes(), NEW)?,
            hostname: cstr(spec.hostname.as_bytes(), "the hostname")?,
            cwd: path_c(&cwd)?,
            candidates: candidates(spec)?,
            argv: spec
                .argv
                .iter()
                .map(|a| cstr(a.as_bytes(), "an argument"))
                .collect::<Result<_, _>>()?,
            envp: spec
                .env
                .iter()
                .map(|(k, v)| {
                    let mut kv = k.as_bytes().to_vec();
                    kv.push(b'=');
                    kv.extend_from_slice(v.as_bytes());
                    cstr(&kv, "an environment variable")
                })
                .collect::<Result<_, _>>()?,
            landlock: landlock(landlock_mode(spec), rules)?,
            seccomp: seccomp_filters(spec.seccomp)?,
            cap_last,
            env_area,
            arg_area,
        })
    }
}
