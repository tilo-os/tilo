//! Everything the job's init needs, prepared by the worker.
//!
//! This module turns a [`JobSpec`] into C strings, mount operations, Landlock
//! rules and BPF programs: the worker reads the host's files and makes the
//! idmapped mounts, and the init (a new program in the job's namespaces, which
//! reads this plan from the worker) only makes system calls.

use std::ffi::{CString, OsStr};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::sys;
use crate::job::SpawnError;
use crate::job::spec::{JobSpec, Landlock, MountKind, Root, Seccomp, Tmp};

/// Where the old root is reachable while the new one is built.
const OLD: &str = "/oldroot";
/// Where the new root is built.
const NEW: &str = "/newroot";

/// One step of building the job's filesystem.
#[derive(Debug, Serialize, Deserialize)]
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
    /// Create a file in the staging root with `contents`, root's where the
    /// job's namespace maps root, mode 0644.
    Write {
        path: CString,
        contents: Vec<u8>,
    },
}

impl Op {
    /// Whether a failure of this step stops the job.
    pub fn fatal(&self) -> bool {
        matches!(
            self,
            Op::Mount { .. }
                | Op::SetAttr { .. }
                | Op::Symlink { .. }
                | Op::Attach { .. }
                | Op::Write { .. }
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
            Op::Write { path, .. } => format!("writing {}", inside(path)),
        }
    }
}

/// A Landlock rule: a path inside the new root and the rights beneath it.
#[derive(Debug, Serialize, Deserialize)]
pub struct Rule {
    pub path: CString,
    pub access: u64,
}

/// What the init does, all of it prepared. The worker's descriptors and its
/// own notes stay with the worker: the init finds the descriptors at the
/// numbers its `Control` names.
#[derive(Debug, Serialize, Deserialize)]
pub struct Plan {
    /// The directory the staging tmpfs is mounted on (an existing one).
    pub stage: CString,
    pub stage_old: CString,
    pub stage_new: CString,
    pub ops: Vec<Op>,
    /// Detached idmapped mounts for [`Op::Attach`], made by the worker.
    #[serde(skip)]
    pub trees: Vec<OwnedFd>,
    /// Writable host paths bound without an idmap (their filesystem can't take one): the job,
    /// as a user of its own, may not be able to write their owner's files.
    #[serde(skip)]
    pub unmapped: Vec<PathBuf>,
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
    /// The Landlock ruleset, when the job runs under Landlock.
    #[serde(skip)]
    pub landlock: Option<OwnedFd>,
    pub rules: Vec<Rule>,
    /// BPF programs (`struct sock_filter` arrays, as bytes), installed in
    /// order.
    pub seccomp: Vec<Vec<u8>>,
    pub cap_last: u32,
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

/// Whose files an idmapped bind shows as whose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owners {
    /// The worker's files are the job's own: what it works on.
    Job,
    /// Every id keeps its number: the job's system files (its image, the
    /// host's `/usr`), whose root-owned files stay root's, as programs that
    /// trust only root's files (a loader, a setuid helper) check.
    System,
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
    /// With a block: the user namespace of [`Owners::System`] binds.
    system_userns: Option<OwnedFd>,
    /// The job's own host uid and gid, with a block.
    job_ids: Option<(u32, u32)>,
    unmapped: Vec<PathBuf>,
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
        owners: Owners,
    ) -> Result<(), SpawnError> {
        self.point(inside, file)?;
        let dst = under(NEW, inside)?;
        self.bind_at(real, dst, libc::MS_BIND | libc::MS_REC, idmap, owners)
    }

    /// Bind the host path `real` at `dst` (a path in the staging root):
    /// idmapped where the job has host ids of its own, plainly otherwise.
    fn bind_at(
        &mut self,
        real: &Path,
        dst: CString,
        flags: libc::c_ulong,
        idmap: Idmap,
        owners: Owners,
    ) -> Result<(), SpawnError> {
        let socket = std::fs::metadata(real).is_ok_and(|m| {
            use std::os::unix::fs::FileTypeExt as _;
            m.file_type().is_socket()
        });
        let userns = match owners {
            Owners::Job => &self.userns,
            Owners::System => &self.system_userns,
        };
        if let (Some(userns), Some((uid, gid))) = (userns, self.job_ids) {
            if socket {
                // A socket the caller made for this job (a VM step's passt): it becomes the
                // job's, which connecting to it takes, wherever it lives. Never idmapped: it
                // often sits on a filesystem that can't be (a container's /tmp).
                if std::os::unix::fs::chown(real, Some(uid), Some(gid)).is_err()
                    && idmap == Idmap::Required
                {
                    self.unmapped.push(real.to_path_buf());
                }
            } else {
                match super::idmap::tree(real, userns.as_fd()) {
                    Ok(tree) => {
                        self.ops.push(Op::Attach {
                            tree: self.trees.len(),
                            dst,
                        });
                        self.trees.push(tree);
                        return Ok(());
                    }
                    // Bound as it is: the job still starts, and the caller says which.
                    Err(_) if idmap == Idmap::Required => self.unmapped.push(real.to_path_buf()),
                    Err(_) => {}
                }
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
                    Owners::System,
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
                    self.bind(
                        &src,
                        Path::new(top),
                        false,
                        Idmap::IfPossible,
                        Owners::System,
                    )?;
                    self.attr(Path::new(top), RO, true)?;
                }
                for name in HOST_LINKS {
                    self.host_link(name)?;
                }
                for p in extra {
                    let inside = normal(p, "extra root path")?;
                    let (src, meta) = source(p)?;
                    self.bind(
                        &src,
                        &inside,
                        !meta.is_dir(),
                        Idmap::IfPossible,
                        Owners::System,
                    )?;
                    self.attr(&inside, RO, true)?;
                }
            }
        }
        Ok(())
    }

    /// A passwd entry (and a group entry) for the job's own ids where the
    /// root it runs in has none, as for a nested job running as
    /// [`crate::job::NESTING_ID`] in an image that only knows root: programs
    /// that look themselves up (a login shell, `id -un`, ssh, ush) would
    /// otherwise refuse. The image's files with the line added are written
    /// to the staging root and bound read-only over its own; a root without
    /// these files, or with links where they are, is left as it is.
    fn account(&mut self, root: &Path, uid: u32, gid: u32) -> Result<(), SpawnError> {
        let Some(passwd) = system_file(root, "etc/passwd") else {
            return Ok(());
        };
        let group = system_file(root, "etc/group");
        let Some((passwd, group)) = account_files(&passwd, group.as_deref(), uid, gid) else {
            return Ok(());
        };
        let files = std::iter::once(("passwd", passwd)).chain(group.map(|g| ("group", g)));
        for (name, contents) in files {
            let staged = cstr(format!("/{name}").as_bytes(), name)?;
            let inside = Path::new("/etc").join(name);
            self.ops.push(Op::Write {
                path: staged.clone(),
                contents,
            });
            self.ops.push(Op::Mount {
                src: Some(staged),
                dst: under(NEW, &inside)?,
                fstype: None,
                flags: libc::MS_BIND,
                data: None,
            });
            self.attr(&inside, RO, false)?;
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
                self.bind(&src, &host, false, Idmap::IfPossible, Owners::System)?;
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
            self.bind_at(&d, dst, libc::MS_BIND, Idmap::IfPossible, Owners::Job)?;
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
                self.bind(&src, tmp, false, Idmap::Required, Owners::Job)?;
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
                self.bind(
                    &src,
                    &inside,
                    !meta.is_dir(),
                    Idmap::IfPossible,
                    Owners::Job,
                )?;
                self.attr(&inside, RO, true)?;
            }
            MountKind::ReadWrite(p) => {
                let (src, meta) = source(p)?;
                self.bind(&src, &inside, !meta.is_dir(), Idmap::Required, Owners::Job)?;
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
            MountKind::Overlay {
                lowers,
                upper,
                work,
            } => {
                let opts = self.overlay_options(lowers, Some((upper, work)))?;
                self.fs(
                    "overlay",
                    &inside,
                    libc::MS_NOSUID | libc::MS_NODEV,
                    Some(&opts),
                )?;
                self.writable(&inside, true)?;
            }
            MountKind::Layers { lowers } => {
                if lowers.len() < 2 {
                    return Err(SpawnError::Invalid(format!(
                        "read-only layers at {} need at least two; one is a read-only bind",
                        inside.display()
                    )));
                }
                let opts = self.overlay_options(lowers, None)?;
                self.fs(
                    "overlay",
                    &inside,
                    libc::MS_NOSUID | libc::MS_NODEV | libc::MS_RDONLY,
                    Some(&opts),
                )?;
            }
        }
        Ok(())
    }
}

/// An overlay's upper and work directories, for one the job writes to.
type Writes<'a> = Option<(&'a PathBuf, &'a PathBuf)>;

/// An overlay's layers as its options name them: the lowers, and the upper
/// and work directories.
type Named = (Vec<PathBuf>, Option<(PathBuf, PathBuf)>);

impl Builder {
    /// overlayfs `-o` options for `lowers` (the first on top) and, unless
    /// read-only, the upper and work directories `writes` names.
    ///
    /// With a block of ids each layer is an idmapped mount in the staging
    /// root, which the options name: overlayfs creates what the job writes
    /// with the job's ids, which only an idmapped upper layer stores as the
    /// owner's. Without a block the options name the layers through the old
    /// root.
    ///
    /// `volatile`: what a job writes to an overlay is disposable, so
    /// overlayfs never syncs the upper layer's filesystem, which it otherwise
    /// does when the job's mount namespace goes and which waits for
    /// everything dirty on that volume (gigabytes from the jobs' builds
    /// beside it).
    fn overlay_options(
        &mut self,
        lowers: &[PathBuf],
        writes: Writes<'_>,
    ) -> Result<String, SpawnError> {
        use std::fmt::Write as _;
        if lowers.is_empty() {
            return Err(SpawnError::Invalid(
                "an overlay without a lower layer".into(),
            ));
        }
        let real = |p: &Path| {
            std::fs::canonicalize(p)
                .map_err(|e| SpawnError::Invalid(format!("{}: {e}", p.display())))
        };
        let lowers = lowers
            .iter()
            .map(|l| real(l))
            .collect::<Result<Vec<_>, _>>()?;
        let writes = match writes {
            Some((upper, work)) => Some((real(upper)?, real(work)?)),
            None => None,
        };
        let (lowers, writes) = if self.userns.is_some() {
            self.idmapped_layers(&lowers, writes)?
        } else {
            let old = |p: &Path| PathBuf::from(format!("{OLD}{}", p.display()));
            (
                lowers.iter().map(|l| old(l)).collect(),
                writes.map(|(u, w)| (old(&u), old(&w))),
            )
        };
        let shown = |p: &Path| {
            let shown = p.display().to_string();
            if shown.contains([',', ':', '\\']) {
                return Err(SpawnError::Invalid(format!(
                    "overlay path {shown} contains a comma, colon or backslash"
                )));
            }
            Ok(shown)
        };
        let lowerdir = lowers
            .iter()
            .map(|l| shown(l))
            .collect::<Result<Vec<_>, _>>()?
            .join(":");
        let mut opts = String::from("userxattr");
        if let Some((upper, work)) = writes {
            let _ = write!(
                opts,
                ",volatile,upperdir={},workdir={}",
                shown(&upper)?,
                shown(&work)?
            );
        }
        let _ = write!(opts, ",lowerdir={lowerdir}");
        Ok(opts)
    }

    /// The layers bound idmapped under `/layers/<n>` in the staging root,
    /// named as the overlay sees them there. The upper and work directories
    /// must share a mount, so their common parent is the one idmapped.
    fn idmapped_layers(
        &mut self,
        lowers: &[PathBuf],
        writes: Option<(PathBuf, PathBuf)>,
    ) -> Result<Named, SpawnError> {
        let at = PathBuf::from(format!("/layers/{}", self.overlays));
        self.overlays = self.overlays.saturating_add(1);
        for dir in [Path::new("/layers"), &at] {
            self.ops.push(Op::Mkdir(path_c(dir)?));
        }
        let mut named = Vec::with_capacity(lowers.len());
        for (i, lower) in lowers.iter().enumerate() {
            let lower_at = at.join(format!("lower{i}"));
            self.ops.push(Op::Mkdir(path_c(&lower_at)?));
            self.bind_at(
                lower,
                path_c(&lower_at)?,
                libc::MS_BIND | libc::MS_REC,
                Idmap::Required,
                Owners::Job,
            )?;
            named.push(lower_at);
        }
        let Some((upper, work)) = writes else {
            return Ok((named, None));
        };
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
        let upper_at = at.join("upper");
        self.ops.push(Op::Mkdir(path_c(&upper_at)?));
        self.bind_at(
            &common,
            path_c(&upper_at)?,
            libc::MS_BIND | libc::MS_REC,
            Idmap::Required,
            Owners::Job,
        )?;
        let (upper, work) = (
            upper_at.join(beneath(&upper)),
            upper_at.join(beneath(&work)),
        );
        Ok((named, Some((upper, work))))
    }
}

/// The largest passwd or group file [`Builder::account`] copies: the
/// staging tmpfs holds both.
const ACCOUNT_FILE_MAX: u64 = 512 * 1024;

/// A regular file beneath `root`, read without following any link (an
/// image's link would name a host path to the worker), or `None`.
fn system_file(root: &Path, rel: &str) -> Option<Vec<u8>> {
    use rustix::fs::{Mode, OFlags, ResolveFlags};
    use std::io::Read as _;
    let dir = std::fs::File::open(root).ok()?;
    let fd = rustix::fs::openat2(
        &dir,
        rel,
        // Non-blocking: a FIFO in its place must not hold the worker up.
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    )
    .ok()?;
    let file = std::fs::File::from(fd);
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > ACCOUNT_FILE_MAX {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(ACCOUNT_FILE_MAX).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// `passwd` with an entry for `uid` added, and `group` with one for `gid`
/// where it has none; `None` when `passwd` has `uid` already. The entry is
/// `job` (or `job<uid>` where the image has a `job`), at home in `/tmp`
/// (the job's `HOME`) with root's shell.
fn account_files(
    passwd: &[u8],
    group: Option<&[u8]>,
    uid: u32,
    gid: u32,
) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
    let entries = |file: &[u8]| -> Vec<Vec<Vec<u8>>> {
        file.split(|b| *b == b'\n')
            .map(|line| line.split(|b| *b == b':').map(<[u8]>::to_vec).collect())
            .collect()
    };
    let id_is =
        |e: &Vec<Vec<u8>>, id: u32| e.get(2).is_some_and(|f| *f == id.to_string().as_bytes());
    let users = entries(passwd);
    if users.iter().any(|e| id_is(e, uid)) {
        return None;
    }
    let groups = group.map(entries).unwrap_or_default();
    let taken = |name: &[u8]| {
        users
            .iter()
            .chain(groups.iter())
            .any(|e| e.first().is_some_and(|n| n == name))
    };
    let name = if taken(b"job") {
        format!("job{uid}")
    } else {
        "job".to_owned()
    };
    let shell = users
        .iter()
        .find(|e| id_is(e, 0))
        .and_then(|e| e.get(6))
        .filter(|s| !s.is_empty())
        .map_or_else(|| b"/bin/sh".to_vec(), Clone::clone);
    let append = |file: &[u8], line: &[u8]| {
        let mut out = file.to_vec();
        if !out.is_empty() && !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        out.extend_from_slice(line);
        out.push(b'\n');
        out
    };
    let mut line = format!("{name}:x:{uid}:{gid}:job:/tmp:").into_bytes();
    line.extend_from_slice(&shell);
    let passwd = append(passwd, &line);
    let group = group
        .filter(|_| !groups.iter().any(|e| id_is(e, gid)))
        .map(|g| append(g, format!("{name}:x:{gid}:").as_bytes()));
    Some((passwd, group))
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
type Filesystem = (Vec<Op>, Vec<Rule>, Vec<OwnedFd>, Vec<PathBuf>);

fn filesystem(spec: &JobSpec) -> Result<Filesystem, SpawnError> {
    let userns = match spec.ids.block {
        Some(_) => Some(super::idmap::namespace(&spec.ids).map_err(|e| {
            SpawnError::Setup(format!(
                "a user namespace mapping the job's files to its own ids: {e}"
            ))
        })?),
        None => None,
    };
    let system_userns = match spec.ids.block {
        Some(_) => Some(super::idmap::system_namespace(&spec.ids).map_err(|e| {
            SpawnError::Setup(format!(
                "a user namespace showing the job's system files as they are: {e}"
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
        system_userns,
        job_ids: spec.ids.block.and_then(|b| {
            Some((
                b.uid.checked_add(spec.ids.inside_uid)?,
                b.gid.checked_add(spec.ids.inside_gid)?,
            ))
        }),
        unmapped: Vec::new(),
        trees: Vec::new(),
        overlays: 0,
    };
    b.root(&spec.root)?;
    let root_dir = match &spec.root {
        Root::Image(dir) => dir.as_path(),
        Root::Host { .. } => Path::new("/"),
    };
    b.account(root_dir, spec.ids.inside_uid, spec.ids.inside_gid)?;
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
    Ok((b.ops, b.rules, b.trees, b.unmapped))
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

fn seccomp_filters(mode: Seccomp) -> Result<Vec<Vec<u8>>, SpawnError> {
    use sandbox_core::seccomp;
    let Seccomp::Ci(profile) = mode else {
        return Ok(Vec::new());
    };
    let compile = |e: seccomp::SeccompError| SpawnError::Invalid(format!("seccomp: {e}"));
    let programs = [
        seccomp::clone3_compat_program().map_err(compile)?,
        seccomp::ci_program(profile).map_err(compile)?,
    ];
    Ok(programs.iter().map(seccomp::program_bytes).collect())
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
        let (ops, rules, trees, unmapped) = filesystem(spec)?;
        let stage = stage_dir();
        let (landlock, rules) = landlock(landlock_mode(spec), rules)?.unzip();
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
            unmapped,
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
            landlock,
            rules: rules.unwrap_or_default(),
            seccomp: seccomp_filters(spec.seccomp)?,
            cap_last,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::account_files;

    const PASSWD: &[u8] = b"root:x:0:0:root:/root:/bin/bash\nbin:x:1:1:bin:/bin:/sbin/nologin\n";
    const GROUP: &[u8] = b"root:x:0:\nbin:x:1:\n";

    fn text(b: &[u8]) -> &str {
        std::str::from_utf8(b).unwrap()
    }

    #[test]
    fn a_job_without_an_entry_gets_one_with_roots_shell_and_its_group() {
        let (passwd, group) = account_files(PASSWD, Some(GROUP), 1000, 1000).unwrap();
        assert_eq!(
            text(&passwd).lines().last(),
            Some("job:x:1000:1000:job:/tmp:/bin/bash")
        );
        assert_eq!(text(&group.unwrap()).lines().last(), Some("job:x:1000:"));
    }

    #[test]
    fn a_job_with_an_entry_keeps_the_files() {
        assert!(account_files(PASSWD, Some(GROUP), 1, 1).is_none());
    }

    #[test]
    fn an_existing_group_and_name_are_kept_apart() {
        let passwd = b"root:x:0:0::/root:\njob:x:5:5::/:/bin/sh";
        let (passwd, group) = account_files(passwd, Some(b"users:x:1000:"), 1000, 1000).unwrap();
        // No newline at the end of the image's file, an empty root shell.
        assert_eq!(
            text(&passwd),
            "root:x:0:0::/root:\njob:x:5:5::/:/bin/sh\njob1000:x:1000:1000:job:/tmp:/bin/sh\n"
        );
        assert!(group.is_none(), "the group 1000 exists");
    }
}
