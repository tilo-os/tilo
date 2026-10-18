//! The system read profile as one pre-built, read-only view.
//!
//! Every sandbox mounts the same fixed host paths read-only
//! ([`crate::profile`]). Bound one by one that is ~40 bubblewrap operations
//! per launch, each a mount, a remount and a reread of the mount table: about
//! half of bubblewrap's setup time. The paths are the same for every app and
//! every person, so they are assembled once, as a tree of read-only bind
//! mounts laid out exactly as the app sees them, and a launch binds the tree's
//! top-level directories (`/usr`, `/etc`, `/lib`, `/lib64`) instead.
//!
//! Nothing is copied: every leaf of the view is a bind of the same live host
//! path the per-path bind would have mounted, so a file's bytes, and the
//! composefs verification of the image's `/usr`, are exactly the host's. A
//! composefs image of its own would need copies of those bytes in a verity
//! object store (`/usr/lib` alone is gigabytes), or a further overlay over the
//! merged `/usr`, which is already at overlayfs's stacking limit.
//!
//! **Who builds it.** `tilo-launcher sandbox-view build`, run as root by
//! `tilo-sandbox-view.service` at boot, after an extension merge replaces
//! `/usr`, and whenever a bound path under `/etc` is replaced
//! (`tilo-sandbox-view.path`). It mounts under [`DEFAULT_ROOT`] in the host
//! namespace, which is why it needs root; a launch needs nothing new.
//!
//! **Layout.** `<root>/current` names the current generation `g<id>`. A
//! generation holds one view per network shape, `plain/` and `net/`, because
//! the profile varies only there (the resolver files of
//! [`crate::profile::NETWORK_READ_PATHS`]), and a `manifest` of the identity
//! (device, inode) of every bound object.
//!
//! **What a launch sees.** The same paths, types, modes and contents, with
//! one difference: the directories bubblewrap creates as parents of the
//! binds (`/usr`, `/usr/share`, `/etc`, ...) are, per path, owned by the app
//! in bubblewrap's writable root tmpfs; in the view they are root-owned,
//! `0755` and read-only. The app reads and traverses them as before and can
//! no longer create entries in them. `tests/system_view.rs` lists the visible
//! filesystem both ways and allows exactly that.
//!
//! **Never stale, never partial.** A generation is complete and read-only
//! before `current` is renamed onto it. A launch holds a shared lock on the
//! generation it uses until its sandbox exits; a superseded generation is
//! unmounted once no launch holds it ([`retire`]). A launch whose host paths
//! no longer have the identities the manifest recorded, or whose mounts the
//! view cannot carry unchanged, binds per path as before ([`attach`]).

use std::ffi::{CString, OsStr};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use advisory_lock::{FileLock, TryLockError};

use crate::profile::{GPU_DEVICE_PATHS, NETWORK_READ_PATHS, SYSTEM_READ_PATHS};

/// Where the root service keeps the view.
pub const DEFAULT_ROOT: &str = "/run/tilo-sandbox-view";
/// The symlink naming the current generation.
const CURRENT: &str = "current";
/// Identities of the bound objects, in each generation.
const MANIFEST: &str = "manifest";
/// First manifest line, before the profile digest.
const FORMAT: &str = "tilo-sandbox-view 1";

/// Profile entries the kernel keeps live: `/sys` gains and loses submounts
/// at runtime, so it is bound per launch, as before.
const LIVE_READ_PATHS: &[&str] = &["/sys"];

/// Files every launch mounts its own content over (the synthetic account
/// database). The view carries only an empty mount point there.
const LAUNCH_FILES: &[&str] = &["/etc/passwd"];

/// Which view a launch uses: the profile differs only by whether the launch
/// shares the host network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Plain,
    Net,
}

impl Variant {
    pub const ALL: [Self; 2] = [Self::Plain, Self::Net];

    #[must_use]
    pub const fn dir(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Net => "net",
        }
    }

    /// Paths where a launch of this shape mounts a file of its own, fresh
    /// for each launch: the account database, and the resolver files a
    /// network change rewrites.
    pub fn launch_files(self) -> impl Iterator<Item = &'static str> {
        let network: &[&str] = match self {
            Self::Plain => &[],
            Self::Net => NETWORK_READ_PATHS,
        };
        LAUNCH_FILES.iter().chain(network).copied()
    }
}

/// The profile entries the view holds.
pub fn view_read_paths() -> impl Iterator<Item = &'static str> + Clone {
    SYSTEM_READ_PATHS
        .iter()
        .copied()
        .filter(|path| !LIVE_READ_PATHS.contains(path))
}

/// Digest of everything that decides a view's shape. A launcher built with a
/// different profile than the builder's never uses the builder's view.
fn profile_digest() -> String {
    let mut hasher = blake3::Hasher::new();
    for list in [
        &[FORMAT][..],
        SYSTEM_READ_PATHS,
        LIVE_READ_PATHS,
        LAUNCH_FILES,
        NETWORK_READ_PATHS,
        GPU_DEVICE_PATHS,
    ] {
        for path in list {
            hasher.update(path.as_bytes());
            hasher.update(b"\0");
        }
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

/// A bound object's device and inode, or `None` when the path does not
/// exist (and `--ro-bind-try` would have skipped it).
type Identity = Option<(u64, u64)>;

/// What `stat` reports for `path`, following symlinks as a bind does.
fn host_identity(path: &str) -> io::Result<Identity> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(Some((meta.dev(), meta.ino()))),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

// ---- kernel interfaces ------------------------------------------------------

/// `struct mount_attr`.
#[repr(C)]
struct MountAttr {
    attr_set: u64,
    attr_clr: u64,
    propagation: u64,
    userns_fd: u64,
}

const MOUNT_ATTR_RDONLY: u64 = 0x1;
const MOUNT_ATTR_NOSUID: u64 = 0x2;
const MOUNT_ATTR_NODEV: u64 = 0x4;
const AT_RECURSIVE: libc::c_uint = 0x8000;
const OPEN_TREE_CLONE: libc::c_uint = 1;
const MOVE_MOUNT_F_EMPTY_PATH: libc::c_uint = 0x4;
const MOVE_MOUNT_T_EMPTY_PATH: libc::c_uint = 0x40;

fn cpath(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}

fn cname(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}

fn check(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn owned(fd: libc::c_long) -> io::Result<OwnedFd> {
    let fd = RawFd::try_from(fd).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a non-negative descriptor the kernel just returned to us alone.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// A detached recursive copy of the mount tree at `source` (symlinks
/// followed, as a bind follows them), or `None` when `source` is absent.
fn clone_tree(source: &str) -> io::Result<Option<OwnedFd>> {
    let path = cpath(Path::new(source))?;
    // SAFETY: a live C string; open_tree returns a new descriptor or -1.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_open_tree,
            libc::AT_FDCWD,
            path.as_ptr(),
            OPEN_TREE_CLONE | libc::O_CLOEXEC.unsigned_abs() | AT_RECURSIVE,
        )
    };
    match owned(fd) {
        Ok(fd) => Ok(Some(fd)),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Make every mount at and below `fd` (or `dir_fd`/`path`) read-only,
/// `nosuid`, `nodev` and private: the flags a `--ro-bind` gives, with no
/// propagation back to the host.
fn seal(dir_fd: RawFd, path: &CString, flags: libc::c_uint) -> io::Result<()> {
    let attr = MountAttr {
        attr_set: MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV,
        attr_clr: 0,
        propagation: libc::MS_PRIVATE,
        userns_fd: 0,
    };
    // SAFETY: valid descriptor and C string; `attr` outlives the call and its
    // size is passed alongside.
    let result = unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            dir_fd,
            path.as_ptr(),
            flags | AT_RECURSIVE,
            &raw const attr,
            std::mem::size_of::<MountAttr>(),
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn attach_tree(tree: &OwnedFd, target: &OwnedFd) -> io::Result<()> {
    // SAFETY: two live descriptors and empty C strings.
    let result = unsafe {
        libc::syscall(
            libc::SYS_move_mount,
            tree.as_raw_fd(),
            c"".as_ptr(),
            target.as_raw_fd(),
            c"".as_ptr(),
            MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    // SAFETY: zeroed is a valid `stat`; the kernel fills it for a live fd.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    check(unsafe { libc::fstat(fd, &raw mut st) })?;
    Ok(st)
}

const fn is_dir(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFDIR
}

/// Open `name` in `dir` without following a symlink, as an `O_PATH` handle.
fn open_path(dir: RawFd, name: &OsStr, directory: bool) -> io::Result<OwnedFd> {
    let name = cname(name)?;
    let flags = libc::O_PATH
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: live dirfd and C string.
    owned(libc::c_long::from(unsafe {
        libc::openat(dir, name.as_ptr(), flags)
    }))
}

/// The plain components of an absolute profile path.
fn components(path: &str) -> io::Result<Vec<&OsStr>> {
    let relative = Path::new(path)
        .strip_prefix("/")
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            _ => Err(io::Error::from(io::ErrorKind::InvalidInput)),
        })
        .collect()
}

// ---- building (root) ----------------------------------------------------------

/// Create the mount point for `path` under the view root `root`, parents
/// first (`0755`), the way bubblewrap creates a bind's destination, and
/// return a handle on it. Never follows a symlink: every component is a
/// directory this build made or a directory of a bind already in place.
fn mount_point(root: &OwnedFd, path: &str, directory: bool) -> io::Result<OwnedFd> {
    let parts = components(path)?;
    let (name, parents) = parts
        .split_last()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut dir = open_path(root.as_raw_fd(), OsStr::new("."), true)?;
    for part in parents {
        dir = match open_path(dir.as_raw_fd(), part, true) {
            Ok(next) => next,
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                make_dir(&dir, part)?;
                open_path(dir.as_raw_fd(), part, true)?
            }
            Err(error) => return Err(error),
        };
    }
    let cname = cname(name)?;
    if directory {
        if let Err(error) = make_dir(&dir, name)
            && error.raw_os_error() != Some(libc::EEXIST)
        {
            return Err(error);
        }
    } else {
        // SAFETY: live dirfd and C string; the new descriptor is closed below.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                cname.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o444,
            )
        };
        match owned(libc::c_long::from(fd)) {
            Ok(file) => drop(file),
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {}
            Err(error) => return Err(error),
        }
    }
    let target = open_path(dir.as_raw_fd(), name, false)?;
    let st = fstat(target.as_raw_fd())?;
    let kind_matches = if directory {
        is_dir(&st)
    } else {
        st.st_mode & libc::S_IFMT == libc::S_IFREG
    };
    if !kind_matches {
        return Err(io::Error::other(format!(
            "{path}: the view already holds something else there"
        )));
    }
    Ok(target)
}

fn make_dir(dir: &OwnedFd, name: &OsStr) -> io::Result<()> {
    let cname = cname(name)?;
    // SAFETY: live dirfd and C string.
    check(unsafe { libc::mkdirat(dir.as_raw_fd(), cname.as_ptr(), 0o755) })?;
    // The mode a parent gets is not the builder's umask's to decide.
    // SAFETY: as above; the directory was just created by us.
    check(unsafe { libc::fchmodat(dir.as_raw_fd(), cname.as_ptr(), 0o755, 0) })
}

/// Bind every present view entry, then the launch-file mount points, into the
/// empty view root `root`. Returns each entry's identity, taken from the very
/// object bound.
fn assemble(root: &OwnedFd, variant: Variant) -> io::Result<Vec<(&'static str, Identity)>> {
    let mut identities = Vec::new();
    for path in view_read_paths() {
        let Some(tree) = clone_tree(path)? else {
            identities.push((path, None));
            continue;
        };
        seal(
            tree.as_raw_fd(),
            &CString::default(),
            libc::AT_EMPTY_PATH.unsigned_abs(),
        )?;
        let st = fstat(tree.as_raw_fd())?;
        let target = mount_point(root, path, is_dir(&st))?;
        attach_tree(&tree, &target)?;
        identities.push((path, Some((st.st_dev, st.st_ino))));
    }
    for path in variant.launch_files() {
        mount_point(root, path, false)?;
    }
    Ok(identities)
}

fn open_dir(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

/// Whether `meta` is a directory only `owner` can change.
fn trusted(meta: &std::fs::Metadata, owner: u32) -> bool {
    meta.is_dir() && meta.uid() == owner && meta.mode() & 0o022 == 0
}

fn generation_name(name: &OsStr) -> Option<&str> {
    let name = name.to_str()?;
    let id = name.strip_prefix('g')?;
    (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(name)
}

/// What [`build`] leaves behind.
#[derive(Debug)]
pub struct Built {
    /// The generation `current` now names.
    pub generation: String,
    /// Superseded generations still held by a running sandbox. Each goes
    /// away through [`retire`], which waits for the last of them.
    pub busy: Vec<String>,
}

/// Build a new generation under `root`, make it current and unmount the
/// superseded ones nothing holds. Needs `CAP_SYS_ADMIN` over the mount
/// namespace (root, for the host's).
///
/// # Errors
/// An untrusted `root`, or any mount the view needs failing; nothing is made
/// current then, and the partial generation is removed.
pub fn build(root: &Path) -> io::Result<Built> {
    let euid = rustix::process::geteuid().as_raw();
    match std::fs::DirBuilder::new().mode(0o755).create(root) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    if !trusted(&std::fs::symlink_metadata(root)?, euid) {
        return Err(io::Error::other(format!(
            "{} is not a directory only this user can change",
            root.display()
        )));
    }
    // One builder at a time; a launch never takes this lock.
    let root_dir = FileLock::exclusive(open_dir(root)?)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let generation = format!("g{nanos:x}");
    let path = root.join(&generation);
    std::fs::DirBuilder::new().mode(0o755).create(&path)?;
    let built = populate(&path).and_then(|()| publish(root, &root_dir, &generation));
    if let Err(error) = built {
        discard(&path);
        return Err(error);
    }
    let mut busy = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let Some(name) = generation_name(&entry.file_name()).map(str::to_owned) else {
            continue;
        };
        if name != generation && !retire_generation(root, &name, false)? {
            busy.push(name);
        }
    }
    Ok(Built { generation, busy })
}

/// Mount the generation's tmpfs at `path`, assemble both views and seal it.
fn populate(path: &Path) -> io::Result<()> {
    let target = cpath(path)?;
    // SAFETY: live C strings.
    check(unsafe {
        libc::mount(
            c"tmpfs".as_ptr(),
            target.as_ptr(),
            c"tmpfs".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            c"mode=0755,size=1m,nr_inodes=4096".as_ptr().cast(),
        )
    })?;
    // SAFETY: as above. Private before anything is bound below it.
    check(unsafe {
        libc::mount(
            std::ptr::null(),
            target.as_ptr(),
            std::ptr::null(),
            libc::MS_PRIVATE,
            std::ptr::null(),
        )
    })?;
    let generation = owned_dir(path)?;
    let mut recorded: Option<Vec<(&'static str, Identity)>> = None;
    for variant in Variant::ALL {
        make_dir(&generation, OsStr::new(variant.dir()))?;
        let root = open_path(generation.as_raw_fd(), OsStr::new(variant.dir()), true)?;
        let identities = assemble(&root, variant)?;
        if recorded.as_ref().is_some_and(|first| *first != identities) {
            return Err(io::Error::other(
                "a bound path changed while the view was built",
            ));
        }
        recorded = Some(identities);
    }
    let mut manifest = format!("{FORMAT} {}\n", profile_digest());
    for (path, identity) in recorded.unwrap_or_default() {
        let written = match identity {
            Some((dev, ino)) => writeln!(manifest, "{dev} {ino} {path}"),
            None => writeln!(manifest, "- - {path}"),
        };
        written.map_err(io::Error::other)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o444)
        .open(path.join(MANIFEST))?;
    file.write_all(manifest.as_bytes())?;
    drop(file);
    seal(libc::AT_FDCWD, &target, 0)
}

fn owned_dir(path: &Path) -> io::Result<OwnedFd> {
    Ok(OwnedFd::from(open_dir(path)?))
}

/// Point `current` at `generation`, atomically.
fn publish(root: &Path, root_dir: &File, generation: &str) -> io::Result<()> {
    let staged = format!(".{CURRENT}-{generation}");
    std::os::unix::fs::symlink(generation, root.join(&staged))?;
    let staged = cname(OsStr::new(&staged))?;
    let current = cname(OsStr::new(CURRENT))?;
    // SAFETY: live dirfd and C strings.
    check(unsafe {
        libc::renameat(
            root_dir.as_raw_fd(),
            staged.as_ptr(),
            root_dir.as_raw_fd(),
            current.as_ptr(),
        )
    })
}

/// Remove a generation that never became current.
fn discard(path: &Path) {
    if let Ok(target) = cpath(path) {
        // SAFETY: a live C string.
        unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) };
    }
    let _ = std::fs::remove_dir(path);
}

/// Unmount and remove a superseded generation once no launch holds it,
/// waiting for the last one.
///
/// # Errors
/// A malformed name, the current generation, or an unmount failing.
pub fn retire(root: &Path, generation: &str) -> io::Result<()> {
    retire_generation(root, generation, true).map(drop)
}

/// [`retire`], or `Ok(false)` without waiting when a launch still holds it.
fn retire_generation(root: &Path, generation: &str, wait: bool) -> io::Result<bool> {
    if generation_name(OsStr::new(generation)).is_none() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    if std::fs::read_link(root.join(CURRENT)).is_ok_and(|current| current == Path::new(generation))
    {
        return Err(io::Error::other("refusing to retire the current view"));
    }
    let path = root.join(generation);
    let dir = open_dir(&path)?;
    let held = if wait {
        FileLock::exclusive(dir)?
    } else {
        match FileLock::try_exclusive(dir) {
            Ok(held) => held,
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Error(error)) => return Err(error),
        }
    };
    let target = cpath(&path)?;
    // SAFETY: a live C string. EINVAL: nothing mounted there any more.
    if unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) } < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINVAL) {
            return Err(error);
        }
    }
    drop(held);
    std::fs::remove_dir(&path)?;
    Ok(true)
}

// ---- using it (every launch) --------------------------------------------------

/// A generation a launch uses, locked against retirement while held.
#[derive(Debug)]
pub struct SystemView {
    /// `<root>/g<id>`.
    generation: PathBuf,
    identities: Vec<(String, Identity)>,
    /// Shared lock on the generation; released when the launch drops it.
    _held: FileLock,
}

impl SystemView {
    /// The current generation under `root`, if there is a trustworthy one.
    /// Directories must belong to `owner` (root) and be writable by nobody
    /// else.
    ///
    /// # Errors
    /// I/O failures other than an absent view.
    pub fn open(root: &Path, owner: u32) -> io::Result<Option<Self>> {
        let meta = match std::fs::symlink_metadata(root) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if !trusted(&meta, owner) {
            return Ok(None);
        }
        let current = root.join(CURRENT);
        // Each pass that goes round again does so because a builder made a
        // newer generation current meanwhile.
        loop {
            let name = match std::fs::read_link(&current) {
                Ok(name) => name,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            let Some(name) = generation_name(name.as_os_str()).map(str::to_owned) else {
                return Ok(None);
            };
            let still_current =
                || std::fs::read_link(&current).is_ok_and(|now| now == Path::new(&name));
            let generation = root.join(&name);
            let dir = match open_dir(&generation) {
                Ok(dir) => dir,
                Err(error) if error.kind() == io::ErrorKind::NotFound && !still_current() => {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let dir = match FileLock::try_shared(dir) {
                Ok(dir) => dir,
                // Being retired: superseded after we read `current`.
                Err(TryLockError::WouldBlock) if !still_current() => continue,
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(error)) => return Err(error),
            };
            if !still_current() {
                continue;
            }
            if !trusted(&dir.metadata()?, owner) {
                return Ok(None);
            }
            let Some(identities) = read_manifest(&generation)? else {
                return Ok(None);
            };
            return Ok(Some(Self {
                generation,
                identities,
                _held: dir,
            }));
        }
    }

    /// The directory a launch of `variant` binds its top-level entries from.
    #[must_use]
    pub fn variant_root(&self, variant: Variant) -> PathBuf {
        self.generation.join(variant.dir())
    }

    /// Whether every bound object is still what the host path names. A
    /// replaced file (`ldconfig`, a time zone change) or a remerged `/usr`
    /// fails this until the view is rebuilt.
    fn is_fresh(&self) -> bool {
        self.identities
            .iter()
            .all(|(path, recorded)| host_identity(path).is_ok_and(|now| now == *recorded))
    }

    fn present(&self, path: &Path) -> bool {
        self.identities
            .iter()
            .any(|(entry, identity)| identity.is_some() && path.starts_with(entry))
    }

    /// Whether `dest` exists in the `variant` view, resolved without
    /// following any symlink.
    fn holds(&self, variant: Variant, dest: &str) -> bool {
        let Ok(parts) = components(dest) else {
            return false;
        };
        let Ok(mut dir) = owned_dir(&self.variant_root(variant)) else {
            return false;
        };
        let last = parts.len().saturating_sub(1);
        for (index, part) in parts.iter().enumerate() {
            match open_path(dir.as_raw_fd(), part, index < last) {
                Ok(next) => dir = next,
                Err(_) => return false,
            }
        }
        true
    }

    /// The view's top-level directories for `variant`.
    fn tops(&self, variant: Variant) -> io::Result<Vec<String>> {
        let mut tops = Vec::new();
        for entry in std::fs::read_dir(self.variant_root(variant))? {
            let name = entry?.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
            tops.push(format!("/{name}"));
        }
        tops.sort();
        Ok(tops)
    }

    /// Replace `argv`'s per-path system profile with this view, or leave
    /// `argv` untouched and say why not.
    ///
    /// # Errors
    /// The reason the launch keeps its per-path binds.
    pub fn compose(&self, argv: &mut Vec<String>) -> Result<(), &'static str> {
        if !self.is_fresh() {
            return Err("a bound host path changed since the view was built");
        }
        let (block, variant) = find_profile_block(argv).ok_or("no system profile in the argv")?;
        let (ops, _command) = parse_ops(argv).ok_or("an option the view cannot reason about")?;
        let tops = self.tops(variant).map_err(|_| "the view is unreadable")?;
        let in_view = |dest: &str| {
            let dest = Path::new(dest);
            tops.iter().any(|top| dest.starts_with(top))
        };
        if !ops
            .iter()
            .any(|op| op.start == block.start && op.kind == OpKind::Bind)
        {
            return Err("the profile is not on an option boundary");
        }

        let moved = self.check_ops(&ops, &block, variant, &in_view)?;
        let mut replacement = self.replacement(variant, &tops);
        for range in &moved {
            replacement.extend(argv.get(range.clone()).unwrap_or_default().iter().cloned());
        }
        let mut composed = Vec::with_capacity(argv.len());
        for (index, token) in argv.iter().enumerate() {
            if index == block.start {
                composed.append(&mut replacement);
            }
            if block.contains(&index) || moved.iter().any(|range| range.contains(&index)) {
                continue;
            }
            composed.push(token.clone());
        }
        *argv = composed;
        Ok(())
    }

    /// Check every mount of the launch against the view; return the
    /// operations that must move on top of it (the account file).
    fn check_ops(
        &self,
        ops: &[Op],
        block: &std::ops::Range<usize>,
        variant: Variant,
        in_view: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<std::ops::Range<usize>>, &'static str> {
        let mut moved: Vec<std::ops::Range<usize>> = Vec::new();
        for (index, op) in ops.iter().enumerate() {
            let Some(dest) = op.dest.as_deref() else {
                continue;
            };
            if !in_view(dest) || block.contains(&op.start) {
                continue;
            }
            if op.end <= block.start {
                // Per path, a later profile bind hides this mount; in the
                // view the top-level bind hides it. Same result, but only
                // where a present profile entry covers it.
                if self.present(Path::new(dest)) {
                    continue;
                }
                if op.kind == OpKind::Data && LAUNCH_FILES.contains(&dest) {
                    let start = match index.checked_sub(1).and_then(|prev| ops.get(prev)) {
                        Some(prev) if prev.kind == OpKind::Modifier && prev.end == op.start => {
                            prev.start
                        }
                        _ => op.start,
                    };
                    moved.push(start..op.end);
                    continue;
                }
                return Err("a mount before the profile lands where the view would hide it");
            }
            match op.kind {
                OpKind::Bind | OpKind::Data | OpKind::Mount => {
                    if op.kind == OpKind::Bind
                        && op.optional
                        && !op
                            .source
                            .as_deref()
                            .is_some_and(|src| Path::new(src).exists())
                    {
                        continue;
                    }
                    if !self.holds(variant, dest) {
                        return Err("a mount lands on a path the view does not have");
                    }
                }
                OpKind::Create | OpKind::Modifier | OpKind::Other => {
                    return Err("an option would create or change a path inside the view");
                }
            }
        }
        // The view has only an empty mount point at each launch file; one the
        // launch does not mount over would show where per path nothing is.
        for file in variant.launch_files() {
            let mounted = if LAUNCH_FILES.contains(&file) {
                ops.iter().any(|op| {
                    op.kind == OpKind::Data
                        && op.dest.as_deref() == Some(file)
                        && (op.start >= block.end
                            || moved.iter().any(|range| range.contains(&op.start)))
                })
            } else {
                Path::new(file).exists()
            };
            if !mounted {
                return Err("a launch file the view only has a mount point for is not mounted");
            }
        }
        Ok(moved)
    }

    /// The view's binds, then what stays per launch: live `/sys` and the
    /// resolver files. GPU device nodes are not the profile's: a `gpu` app's
    /// are bound outside the profile block and survive the replacement.
    fn replacement(&self, variant: Variant, tops: &[String]) -> Vec<String> {
        let mut replacement = Vec::new();
        let root = self.variant_root(variant);
        for top in tops {
            replacement.push("--ro-bind".to_string());
            replacement.push(root.join(top.trim_start_matches('/')).display().to_string());
            replacement.push(top.clone());
        }
        for path in LIVE_READ_PATHS {
            replacement.extend(["--ro-bind-try", path, path].map(str::to_string));
        }
        // The cgroup hierarchy stays covered on top of the live `/sys`.
        for path in crate::profile::HIDDEN_PATHS {
            replacement.extend(["--tmpfs", path, "--remount-ro", path].map(str::to_string));
        }
        if variant == Variant::Net {
            for path in NETWORK_READ_PATHS {
                replacement.extend(["--ro-bind-try", path, path].map(str::to_string));
            }
        }
        replacement
    }
}

/// The manifest's identities, or `None` when it describes another profile.
fn read_manifest(generation: &Path) -> io::Result<Option<Vec<(String, Identity)>>> {
    let mut text = String::new();
    File::open(generation.join(MANIFEST))?
        .take(64 * 1024)
        .read_to_string(&mut text)?;
    let mut lines = text.lines();
    if lines.next() != Some(&format!("{FORMAT} {}", profile_digest())) {
        return Ok(None);
    }
    let mut identities = Vec::new();
    for line in lines {
        let mut fields = line.splitn(3, ' ');
        let (Some(dev), Some(ino), Some(path)) = (fields.next(), fields.next(), fields.next())
        else {
            return Ok(None);
        };
        let identity = match (dev, ino) {
            ("-", "-") => None,
            (dev, ino) => match (dev.parse(), ino.parse()) {
                (Ok(dev), Ok(ino)) => Some((dev, ino)),
                _ => return Ok(None),
            },
        };
        identities.push((path.to_string(), identity));
    }
    let expected: Vec<&str> = view_read_paths().collect();
    if identities
        .iter()
        .map(|(path, _)| path.as_str())
        .ne(expected)
    {
        return Ok(None);
    }
    Ok(Some(identities))
}

/// Where the per-path profile sits in `argv`, and which view replaces it.
fn find_profile_block(argv: &[String]) -> Option<(std::ops::Range<usize>, Variant)> {
    // Network first: the plain profile is a prefix of the network one.
    for (shares, variant) in [(true, Variant::Net), (false, Variant::Plain)] {
        let mut block = Vec::new();
        crate::profile::push_system_profile(&mut block, shares);
        let mut found = argv
            .windows(block.len())
            .enumerate()
            .filter(|(_, window)| *window == block.as_slice())
            .map(|(start, _)| start);
        if let Some(start) = found.next() {
            if found.next().is_some() {
                return None;
            }
            return Some((start..start.checked_add(block.len())?, variant));
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpKind {
    /// `--bind`, `--ro-bind`, `--dev-bind` (and `-try`), `--bind-fd`.
    Bind,
    /// `--bind-data`, `--ro-bind-data`: bubblewrap's own file bound on DEST.
    Data,
    /// A fresh filesystem or a remount on DEST.
    Mount,
    /// Creates or changes DEST itself: `--dir`, `--file`, `--symlink`,
    /// `--chmod`, `--lock-file`, overlays.
    Create,
    /// `--perms`, `--size`: applies to the next operation.
    Modifier,
    /// Not a filesystem operation.
    Other,
}

#[derive(Debug)]
struct Op {
    start: usize,
    end: usize,
    kind: OpKind,
    optional: bool,
    source: Option<String>,
    dest: Option<String>,
}

/// Split a bubblewrap argv into its options, up to the command. `None` for an
/// option this does not know (or `--args`, which hides more of them).
fn parse_ops(argv: &[String]) -> Option<(Vec<Op>, usize)> {
    let mut ops = Vec::new();
    let mut index = 0usize;
    while let Some(flag) = argv.get(index) {
        if flag == "--" {
            return Some((ops, index.checked_add(1)?));
        }
        if !flag.starts_with("--") {
            return Some((ops, index));
        }
        // (argument count, kind, source argument, destination argument)
        let (count, kind, source, dest): (usize, OpKind, Option<usize>, Option<usize>) =
            match flag.as_str() {
                "--level-prefix"
                | "--unshare-all"
                | "--share-net"
                | "--unshare-user"
                | "--unshare-user-try"
                | "--unshare-ipc"
                | "--unshare-pid"
                | "--unshare-net"
                | "--unshare-uts"
                | "--unshare-cgroup"
                | "--unshare-cgroup-try"
                | "--disable-userns"
                | "--assert-userns-disabled"
                | "--clearenv"
                | "--new-session"
                | "--die-with-parent"
                | "--as-pid-1" => (0, OpKind::Other, None, None),
                "--argv0" | "--userns" | "--userns2" | "--pidns" | "--uid" | "--gid"
                | "--hostname" | "--chdir" | "--unsetenv" | "--sync-fd" | "--exec-label"
                | "--file-label" | "--seccomp" | "--add-seccomp-fd" | "--block-fd"
                | "--userns-block-fd" | "--info-fd" | "--json-status-fd" | "--cap-add"
                | "--cap-drop" => (1, OpKind::Other, None, None),
                "--perms" | "--size" => (1, OpKind::Modifier, None, None),
                "--setenv" => (2, OpKind::Other, None, None),
                "--remount-ro" | "--proc" | "--dev" | "--tmpfs" | "--mqueue" => {
                    (1, OpKind::Mount, None, Some(1))
                }
                "--dir" | "--lock-file" | "--tmp-overlay" | "--ro-overlay" => {
                    (1, OpKind::Create, None, Some(1))
                }
                "--overlay-src" => (1, OpKind::Create, None, None),
                "--bind" | "--ro-bind" | "--dev-bind" | "--bind-try" | "--ro-bind-try"
                | "--dev-bind-try" | "--bind-fd" | "--ro-bind-fd" => {
                    (2, OpKind::Bind, Some(1), Some(2))
                }
                "--bind-data" | "--ro-bind-data" => (2, OpKind::Data, None, Some(2)),
                "--file" | "--symlink" | "--chmod" => (2, OpKind::Create, None, Some(2)),
                "--overlay" => (3, OpKind::Create, None, Some(3)),
                _ => return None,
            };
        let end = index.checked_add(count)?.checked_add(1)?;
        if end > argv.len() {
            return None;
        }
        let arg = |offset: Option<usize>| {
            offset
                .and_then(|offset| index.checked_add(offset))
                .and_then(|at| argv.get(at))
                .cloned()
        };
        ops.push(Op {
            start: index,
            end,
            kind,
            optional: flag.ends_with("-try"),
            source: arg(source),
            dest: arg(dest),
        });
        index = end;
    }
    Some((ops, index))
}

/// Swap `argv`'s per-path system profile for the host's current view when
/// there is one that carries this launch unchanged. Keep the returned
/// handle until the sandbox exits: it keeps the view from being retired.
pub fn attach(argv: &mut Vec<String>) -> Option<SystemView> {
    let view = match SystemView::open(Path::new(DEFAULT_ROOT), 0) {
        Ok(Some(view)) => view,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(%error, "the system view is unreadable; binding the profile per path");
            return None;
        }
    };
    match view.compose(argv) {
        Ok(()) => {
            tracing::debug!(generation = %view.generation.display(), "system profile from the view");
            Some(view)
        }
        Err(reason) => {
            tracing::info!(reason, "binding the system profile per path");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view shaped like a built one, without mounts: the profile entries
    /// that exist on this host as empty directories or files.
    fn fake_view(dir: &Path) -> SystemView {
        let generation = dir.join("g1");
        for variant in Variant::ALL {
            let root = generation.join(variant.dir());
            for path in view_read_paths().chain(variant.launch_files()) {
                let launch_file = variant.launch_files().any(|file| file == path);
                let Ok(meta) = std::fs::metadata(path) else {
                    if launch_file {
                        let dest = root.join(path.trim_start_matches('/'));
                        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                        std::fs::write(dest, b"").unwrap();
                    }
                    continue;
                };
                let dest = root.join(path.trim_start_matches('/'));
                if meta.is_dir() && !launch_file {
                    std::fs::create_dir_all(&dest).unwrap();
                } else {
                    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                    std::fs::write(&dest, b"").unwrap();
                }
            }
        }
        SystemView {
            identities: view_read_paths()
                .map(|path| (path.to_string(), host_identity(path).unwrap()))
                .collect(),
            _held: FileLock::shared(File::open(&generation).unwrap()).unwrap(),
            generation,
        }
    }

    fn launch(shares_net: bool, after: &[&str]) -> Vec<String> {
        let mut argv: Vec<String> = ["--ro-bind-data", "42", "/etc/passwd", "--unshare-all"]
            .map(String::from)
            .to_vec();
        argv.extend(["--tmpfs", "/tmp", "--proc", "/proc", "--dev", "/dev"].map(String::from));
        crate::profile::push_system_profile(&mut argv, shares_net);
        argv.extend(after.iter().map(|arg| (*arg).to_string()));
        argv.push("/var/tilo-apps/app/bin/app".to_string());
        argv
    }

    fn binds(argv: &[String]) -> Vec<(String, String)> {
        argv.windows(3)
            .filter(|w| w[0].contains("bind"))
            .map(|w| (w[0].clone(), w[2].clone()))
            .collect()
    }

    #[test]
    fn a_launch_binds_the_view_tops_instead_of_each_profile_path() {
        let dir = tempfile::tempdir().unwrap();
        let view = fake_view(dir.path());
        let mut argv = launch(false, &["--bind", "/srv/room", "/run/tilo/app"]);
        let before = argv.clone();
        view.compose(&mut argv).unwrap();

        let tops = view.tops(Variant::Plain).unwrap();
        assert!(tops.contains(&"/usr".to_string()) && tops.contains(&"/etc".to_string()));
        for top in &tops {
            let source = view
                .variant_root(Variant::Plain)
                .join(top.trim_start_matches('/'));
            assert!(
                argv.windows(3).any(|w| w[0] == "--ro-bind"
                    && w[1] == source.display().to_string()
                    && w[2] == *top),
                "{top} must come from the view: {argv:?}"
            );
        }
        for path in view_read_paths() {
            assert!(
                !argv
                    .windows(3)
                    .any(|w| w[0] == "--ro-bind-try" && w[2] == path),
                "{path} is the view's now, not a bind of its own: {argv:?}"
            );
        }
        // Kept per launch: /sys (live submounts) and the device nodes.
        let sys = argv
            .windows(3)
            .position(|w| w[0] == "--ro-bind-try" && w[1] == "/sys" && w[2] == "/sys")
            .unwrap();
        // The cgroup hierarchy is still covered, after /sys.
        let hidden = argv
            .windows(4)
            .position(|w| {
                w == [
                    "--tmpfs",
                    "/sys/fs/cgroup",
                    "--remount-ro",
                    "/sys/fs/cgroup",
                ]
            })
            .unwrap();
        assert!(sys < hidden, "{argv:?}");
        // No GPU node: the default profile has none, and the view adds none
        // (a `gpu` app's nodes are bound outside the profile block).
        for device in GPU_DEVICE_PATHS {
            assert!(
                !argv
                    .windows(3)
                    .any(|w| w[0] == "--dev-bind-try" && w[2] == *device)
            );
        }
        // The account file goes on top of the view's /etc, not under it.
        let etc = argv
            .windows(3)
            .position(|w| w[0] == "--ro-bind" && w[2] == "/etc")
            .unwrap();
        let passwd = argv
            .windows(3)
            .position(|w| w[0] == "--ro-bind-data" && w[2] == "/etc/passwd")
            .unwrap();
        assert!(passwd > etc, "{argv:?}");
        // Everything else is untouched, in order.
        assert_eq!(argv.last(), before.last());
        assert!(
            argv.windows(3)
                .any(|w| w[0] == "--bind" && w[1] == "/srv/room" && w[2] == "/run/tilo/app")
        );
        assert!(binds(&argv).len() < binds(&before).len());
    }

    #[test]
    fn a_network_launch_uses_the_network_view_and_fresh_resolver_binds() {
        if !NETWORK_READ_PATHS
            .iter()
            .all(|path| Path::new(path).exists())
        {
            // The resolver files are this host's; without them the network
            // view must refuse, which the next assertion block covers.
            let dir = tempfile::tempdir().unwrap();
            let view = fake_view(dir.path());
            let mut argv = launch(true, &[]);
            assert!(view.compose(&mut argv).is_err());
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let view = fake_view(dir.path());
        let mut argv = launch(true, &[]);
        view.compose(&mut argv).unwrap();
        let net_root = view.variant_root(Variant::Net).display().to_string();
        assert!(
            argv.iter().any(|arg| arg.starts_with(&net_root)),
            "{argv:?}"
        );
        for path in NETWORK_READ_PATHS {
            let bind = argv
                .windows(3)
                .position(|w| w[0] == "--ro-bind-try" && w[2] == *path)
                .unwrap();
            let etc = argv.windows(3).position(|w| w[2] == "/etc").unwrap();
            assert!(bind > etc, "{path} must be mounted over the view: {argv:?}");
        }
    }

    #[test]
    fn launches_the_view_cannot_carry_unchanged_keep_per_path_binds() {
        let dir = tempfile::tempdir().unwrap();
        let view = fake_view(dir.path());
        let refused = |argv: &mut Vec<String>| {
            let before = argv.clone();
            let result = view.compose(argv);
            assert_eq!(*argv, before, "a refusal must leave the argv as it was");
            result.is_err()
        };
        // A bind onto a path the view lacks (the system-authority grant).
        assert!(refused(&mut launch(
            false,
            &["--ro-bind", "/usr/bin", "/usr/bin"]
        )));
        // Creating something inside the view.
        assert!(refused(&mut launch(false, &["--dir", "/usr/share/extra"])));
        assert!(refused(&mut launch(false, &["--symlink", "x", "/etc/y"])));
        // An option this module does not know, or arguments it cannot see.
        assert!(refused(&mut launch(false, &["--args", "9"])));
        assert!(refused(&mut launch(false, &["--some-new-option"])));
        // A mount before the profile that no profile entry hides: per path it
        // stays visible, under the view it would vanish.
        let mut argv = launch(false, &[]);
        argv.splice(
            0..0,
            ["--ro-bind", "/srv/x", "/usr/libexec/x"].map(String::from),
        );
        assert!(refused(&mut argv));
        // No account file: the view's empty mount point would show.
        let mut argv = launch(false, &[]);
        argv.drain(0..3);
        assert!(refused(&mut argv));
        // No profile at all.
        let mut argv = vec!["--unshare-all".to_string(), "/bin/app".to_string()];
        assert!(refused(&mut argv));
    }

    #[test]
    fn a_stale_view_is_never_used() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = fake_view(dir.path());
        let (_, identity) = view
            .identities
            .iter_mut()
            .find(|(_, identity)| identity.is_some())
            .unwrap();
        *identity = Some((0, 0));
        let mut argv = launch(false, &[]);
        let before = argv.clone();
        assert!(view.compose(&mut argv).is_err());
        assert_eq!(argv, before);
    }

    #[test]
    fn a_mount_inside_a_profile_directory_goes_on_top_of_the_view() {
        // GPU narrowing binds one loader manifest inside a profile directory.
        let dir = tempfile::tempdir().unwrap();
        let view = fake_view(dir.path());
        let inside = view
            .variant_root(Variant::Plain)
            .join("usr/share/fonts/one.ttf");
        if !inside.parent().unwrap().is_dir() {
            return;
        }
        std::fs::write(&inside, b"").unwrap();
        let mut argv = launch(
            false,
            &["--ro-bind", "/srv/one.ttf", "/usr/share/fonts/one.ttf"],
        );
        view.compose(&mut argv).unwrap();
        // A -try bind whose source is missing is skipped by bubblewrap, so it
        // needs no mount point.
        let mut argv = launch(
            false,
            &[
                "--ro-bind-try",
                "/nonexistent/x",
                "/usr/share/fonts/missing",
            ],
        );
        view.compose(&mut argv).unwrap();
        // A present source onto a missing destination would fail.
        let mut argv = launch(false, &["--ro-bind", "/srv/two", "/usr/share/fonts/two"]);
        assert!(view.compose(&mut argv).is_err());
    }

    #[test]
    fn the_rebuild_triggers_watch_every_replaceable_view_path() {
        // The launch-side identity check keeps a stale view from being used;
        // the path unit is what makes a fresh one again. Every /etc entry can
        // be replaced on a running system (ldconfig, a time zone change, an
        // admin), so each is watched. /usr changes only with an extension
        // merge, which restarts the build itself.
        let unit = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../os/usr/lib/systemd/system/tilo-sandbox-view.path");
        let text = std::fs::read_to_string(&unit).unwrap();
        for path in view_read_paths().filter(|path| path.starts_with("/etc/")) {
            assert!(
                text.lines()
                    .any(|line| line == format!("PathChanged={path}")),
                "{} must watch {path}",
                unit.display()
            );
        }
        let activate = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../os/usr/lib/systemd/system/tilo-extension-activate.service");
        assert!(
            std::fs::read_to_string(activate)
                .unwrap()
                .contains("tilo-sandbox-view.service"),
            "an extension merge replaces /usr; it must rebuild the view"
        );
    }

    #[test]
    fn the_argv_parser_knows_every_option_the_launcher_emits() {
        let mut argv = launch(true, &[]);
        argv.pop();
        crate::sandbox::push_capability_binds(
            &mut argv,
            &crate::capabilities::CapabilityPlan {
                ro_paths: vec!["/srv/a".into()],
                rw_paths: vec!["/srv/b".into(), "/dev/video0".into()],
                masked_dirs: vec!["/srv/b/.private".into()],
                pinned_dirs: vec!["/srv/b".into()],
                ..Default::default()
            },
        );
        argv.extend(
            [
                "--setenv",
                "A",
                "B",
                "--perms",
                "0444",
                "--remount-ro",
                "/x",
                "--as-pid-1",
            ]
            .map(String::from),
        );
        argv.push("/bin/app".into());
        let (ops, command) = parse_ops(&argv).unwrap();
        assert_eq!(argv.get(command).map(String::as_str), Some("/bin/app"));
        assert!(ops.iter().all(|op| op.end <= command));
    }
}
