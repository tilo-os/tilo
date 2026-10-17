//! Privileged, descriptor-bound projection into an already journalled host.
//! Bubblewrap canonicalizes even --bind-fd operands. These aliases live only in
//! the host's private mount namespace and never expose the snapshot's ancestors.
use super::ProjectionReceipt as Alias;
use super::{failure, process::Incarnation};
use runtime_portal::fd_transport;
use std::os::fd::AsFd;
use std::{
    ffi::CString,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, PermissionsExt},
            net::UnixStream,
        },
    },
    path::Path,
    time::{Duration, Instant},
};

pub(super) struct Projection {
    pub(super) source: File,
    pub(super) output: Option<File>,
    pub(super) scratch: Option<File>,
}

pub(super) struct Directory {
    temporary: Option<tempfile::TempDir>,
    pub(super) alias: Alias,
}
impl Directory {
    pub(super) fn with_inventory_scratch(output: bool) -> io::Result<Self> {
        Self::new(output, true)
    }
    fn new(output: bool, scratch: bool) -> io::Result<Self> {
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let temporary = tempfile::Builder::new()
            .prefix("tilo-capture-projection-")
            .tempdir_in("/tmp")?;
        for name in [
            Some("source"),
            output.then_some("output"),
            scratch.then_some("scratch"),
        ]
        .into_iter()
        .flatten()
        {
            let path = temporary.path().join(name);
            std::fs::create_dir(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o711))?;
        let parent = std::fs::metadata(temporary.path())?;
        let alias = Alias {
            path: temporary.path().into(),
            dev: parent.dev(),
            ino: parent.ino(),
            source: std::fs::metadata(temporary.path().join("source"))?.ino(),
            scratch: scratch
                .then(|| std::fs::metadata(temporary.path().join("scratch")).map(|m| m.ino()))
                .transpose()?,
            output: output
                .then(|| std::fs::metadata(temporary.path().join("output")).map(|m| m.ino()))
                .transpose()?,
        };
        Ok(Self {
            temporary: Some(temporary),
            alias,
        })
    }
    pub(super) fn cleanup(&self) -> io::Result<()> {
        self.alias.cleanup()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        // Custody, not a recursive temporary-directory destructor, decides when
        // aliases may disappear. Unproved hosts retain their protected receipt.
        if let Some(directory) = self.temporary.take() {
            let _ = directory.keep();
        }
    }
}
impl Alias {
    pub fn cleanup(&self) -> io::Result<()> {
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| failure("capture alias name invalid"))?;
        if self.path.parent() != Some(Path::new("/tmp"))
            || !name.starts_with("tilo-capture-projection-")
            || name.len() > 128
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || rustix::process::geteuid().as_raw() != 0
        {
            return Err(failure(
                "capture alias is not a protected native projection",
            ));
        }
        let parent = match std::fs::symlink_metadata(&self.path) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if !parent.is_dir()
            || parent.uid() != 0
            || parent.mode() & 0o777 != 0o711
            || (parent.dev(), parent.ino()) != (self.dev, self.ino)
        {
            return Err(failure("capture alias parent identity changed"));
        }
        for entry in std::fs::read_dir(&self.path)? {
            let entry = entry?;
            let expected = if entry.file_name() == "source" {
                Some(self.source)
            } else if entry.file_name() == "output" {
                self.output
            } else if entry.file_name() == "scratch" {
                self.scratch
            } else {
                None
            };
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if !metadata.is_dir()
                || metadata.uid() != 0
                || metadata.mode() & 0o777 != 0o700
                || metadata.dev() != self.dev
                || Some(metadata.ino()) != expected
            {
                return Err(failure("capture alias child identity changed"));
            }
        }
        for name in ["source", "output", "scratch"] {
            match std::fs::remove_dir(self.path.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        std::fs::remove_dir(&self.path)
    }
}

pub(super) fn project(
    host: Incarnation,
    source: &File,
    output: Option<&File>,
    directory: &Directory,
) -> io::Result<Projection> {
    project_inner(host, source, output, None, directory)
}

pub(super) fn project_with_inventory_scratch(
    host: Incarnation,
    source: &File,
    output: Option<&File>,
    scratch: Option<&File>,
    directory: &Directory,
) -> io::Result<Projection> {
    if scratch.is_none() {
        return project(host, source, output, directory);
    }
    project_inner(host, source, output, scratch, directory)
}

fn project_inner(
    host: Incarnation,
    source: &File,
    output: Option<&File>,
    scratch: Option<&File>,
    directory: &Directory,
) -> io::Result<Projection> {
    if rustix::process::geteuid().as_raw() != 0 || !source.metadata()?.is_dir() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let namespace = host.mount_namespace()?;
    let source_slot = Slot::new(directory, "source", source, true)?;
    let output_slot = output
        .map(|file| Slot::new(directory, "output", file, false))
        .transpose()?;
    let scratch_slot = scratch
        .map(|file| Slot::new(directory, "scratch", file, false))
        .transpose()?;
    let projected_scratch = scratch_slot
        .as_ref()
        .map(|slot| slot.mount(&namespace))
        .transpose()?;
    let projected_source = source_slot.mount(&namespace)?;
    let projected_output = output_slot
        .as_ref()
        .map(|slot| slot.mount(&namespace))
        .transpose()?;
    // The independently authenticated incarnation must still own this namespace.
    let current = host.mount_namespace()?.metadata()?;
    let held = namespace.metadata()?;
    if (current.dev(), current.ino()) != (held.dev(), held.ino()) {
        return Err(failure("capture projection namespace changed"));
    }
    Ok(Projection {
        source: projected_source,
        output: projected_output,
        scratch: projected_scratch,
    })
}

struct Slot<'a> {
    original: &'a File,
    target: File,
    parent_pin: File,
    parent: CString,
    name: CString,
    readonly: bool,
}
impl<'a> Slot<'a> {
    fn new(
        directory: &Directory,
        name: &str,
        original: &'a File,
        readonly: bool,
    ) -> io::Result<Self> {
        if !original.metadata()?.is_dir() {
            return Err(failure("capture projection requires a held directory"));
        }
        let path = directory.alias.path.join(name);
        let target = File::open(&path)?;
        Ok(Self {
            parent_pin: File::open(&directory.alias.path)?,
            parent: CString::new(directory.alias.path.as_os_str().as_bytes())
                .map_err(io::Error::other)?,
            name: CString::new(name).map_err(io::Error::other)?,
            original,
            target,
            readonly,
        })
    }

    fn mount(&self, namespace: &File) -> io::Result<File> {
        let (receiver, sender) = UnixStream::pair()?;
        receiver.set_read_timeout(Some(Duration::from_secs(5)))?;
        let parent = unsafe { libc::getpid() };
        // All operands and buffers are prepared before fork. The child executes
        // only raw syscalls and exits without Rust allocation, locks or drops.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            unsafe {
                libc::close(receiver.as_raw_fd());
                child(self, namespace.as_raw_fd(), sender.as_raw_fd(), parent);
            }
        }
        let mut child = Reap(pid);
        drop(sender);
        let transfer = fd_transport::recv_with_fd(receiver.as_fd());
        let status = child.wait()?;
        let (bytes, descriptor) = transfer?;
        if bytes.len() != 4 {
            return Err(failure("capture projection result frame differs"));
        }
        let errno = i32::from_be_bytes(
            bytes
                .try_into()
                .map_err(|_| failure("capture projection result bound"))?,
        );
        if errno != 0 {
            return Err(io::Error::from_raw_os_error(errno));
        }
        if status != 0 {
            return Err(failure("capture projection child did not finish"));
        }
        let file =
            File::from(descriptor.ok_or_else(|| failure("capture projection descriptor missing"))?);
        let original = self.original.metadata()?;
        let actual = file.metadata()?;
        if !actual.is_dir()
            || (
                actual.dev(),
                actual.ino(),
                actual.uid(),
                actual.gid(),
                actual.mode(),
            ) != (
                original.dev(),
                original.ino(),
                original.uid(),
                original.gid(),
                original.mode(),
            )
        {
            return Err(failure("capture projection changed held source identity"));
        }
        let mut flags: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(file.as_raw_fd(), &raw mut flags) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if flags.f_flag & libc::ST_NOEXEC == 0
            || (self.readonly && flags.f_flag & libc::ST_RDONLY == 0)
        {
            return Err(failure("capture projection mount permissions changed"));
        }
        // Retain the target descriptor until its exact bind operation finishes.
        let _ = self.target.metadata()?;
        Ok(file)
    }
}

#[repr(C)]
struct MountAttributes {
    set: u64,
    clear: u64,
    propagation: u64,
    userns: u64,
}

/// Called only in the post-fork child. Do not add heap allocation, Rust I/O,
/// formatting, mutexes, destructors, unwinding or ambient executable dispatch.
unsafe fn child(slot: &Slot<'_>, namespace: RawFd, socket: RawFd, parent: libc::pid_t) -> ! {
    let mut opened = -1;
    let result = unsafe {
        libc::alarm(5);
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
            *libc::__errno_location()
        } else if libc::getppid() != parent {
            libc::EINTR
        } else {
            // The granted FD belongs to the broker's current mount namespace.
            // Clone and restrict its detached tree before joining the host:
            // open_tree refuses a source from a different mounted namespace.
            let tree = detached(slot);
            if tree < 0 || libc::setns(namespace, libc::CLONE_NEWNS) < 0 {
                *libc::__errno_location()
            } else {
                // Reopen this root-owned immutable parent inside the target
                // namespace. A pre-fork target FD belongs to the old namespace.
                let parent_fd = libc::open(
                    slot.parent.as_ptr(),
                    libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                );
                let target_fd = if parent_fd < 0 {
                    -1
                } else {
                    libc::openat(
                        parent_fd,
                        slot.name.as_ptr(),
                        libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    )
                };
                if target_fd < 0 {
                    *libc::__errno_location()
                } else if !same_target(slot, parent_fd, target_fd) {
                    libc::ESTALE
                } else if libc::syscall(
                    libc::SYS_move_mount,
                    tree,
                    c"".as_ptr(),
                    target_fd,
                    c"".as_ptr(),
                    0x4 | 0x40,
                ) < 0
                {
                    *libc::__errno_location()
                } else {
                    opened = libc::openat(
                        parent_fd,
                        slot.name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    );
                    if opened < 0 {
                        *libc::__errno_location()
                    } else {
                        0
                    }
                }
            }
        }
    };
    let bytes = result.to_be_bytes();
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let mut control = [0usize; 4];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    if result == 0 {
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = unsafe { libc::CMSG_SPACE(4) as _ };
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&raw const message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(4) as _;
            std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), opened);
        }
    }
    unsafe {
        let sent = libc::sendmsg(socket, &raw const message, libc::MSG_NOSIGNAL);
        libc::_exit(i32::from(sent != 4 || result != 0));
    }
}

/// Post-fork detached preparation in the original source mount namespace.
unsafe fn detached(slot: &Slot<'_>) -> libc::c_long {
    unsafe {
        let tree = libc::syscall(
            libc::SYS_open_tree,
            slot.original.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | 0x8000 | 1 | libc::O_CLOEXEC,
        );
        if tree < 0 {
            return -1;
        }
        let attributes = MountAttributes {
            set: 2 | 4 | 8 | u64::from(slot.readonly),
            clear: 0,
            propagation: libc::MS_PRIVATE,
            userns: 0,
        };
        // Restrictions apply before the detached mount is visible to the host.
        if libc::syscall(
            libc::SYS_mount_setattr,
            tree,
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | 0x8000,
            &raw const attributes,
            std::mem::size_of::<MountAttributes>(),
        ) < 0
        {
            -1
        } else {
            tree
        }
    }
}

/// Post-fork identity check against capabilities held before namespace entry.
unsafe fn same_target(slot: &Slot<'_>, parent: RawFd, target: RawFd) -> bool {
    let mut before_parent: libc::stat = unsafe { std::mem::zeroed() };
    let mut after_parent: libc::stat = unsafe { std::mem::zeroed() };
    let mut before_target: libc::stat = unsafe { std::mem::zeroed() };
    let mut after_target: libc::stat = unsafe { std::mem::zeroed() };
    unsafe {
        libc::fstat(slot.parent_pin.as_raw_fd(), &raw mut before_parent) == 0
            && libc::fstat(parent, &raw mut after_parent) == 0
            && libc::fstat(slot.target.as_raw_fd(), &raw mut before_target) == 0
            && libc::fstat(target, &raw mut after_target) == 0
            && before_parent.st_dev == after_parent.st_dev
            && before_parent.st_ino == after_parent.st_ino
            && before_target.st_dev == after_target.st_dev
            && before_target.st_ino == after_target.st_ino
            && after_parent.st_uid == 0
            && after_target.st_uid == 0
            && after_parent.st_mode & 0o777 == 0o711
            && after_target.st_mode & 0o777 == 0o700
    }
}

struct Reap(libc::pid_t);
impl Reap {
    fn wait(&mut self) -> io::Result<i32> {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(6))
            .ok_or_else(|| io::Error::other("capture projection deadline overflow"))?;
        loop {
            let mut status = 0;
            let result = unsafe { libc::waitpid(self.0, &raw mut status, libc::WNOHANG) };
            if result == self.0 {
                self.0 = 0;
                return Ok(status);
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Reap {
    fn drop(&mut self) {
        if self.0 > 0 {
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
                while libc::waitpid(self.0, std::ptr::null_mut(), 0) < 0 {
                    if *libc::__errno_location() != libc::EINTR {
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_cleanup_refuses_unadmitted_paths_without_touching_data() {
        let untouched = tempfile::tempdir().unwrap();
        std::fs::write(untouched.path().join("personal"), b"unchanged").unwrap();
        let metadata = std::fs::metadata(untouched.path()).unwrap();
        let receipt = Alias {
            path: untouched.path().into(),
            dev: metadata.dev(),
            ino: metadata.ino(),
            source: 0,
            output: None,
            scratch: None,
        };
        assert!(receipt.cleanup().is_err());
        assert_eq!(
            std::fs::read(untouched.path().join("personal")).unwrap(),
            b"unchanged"
        );
    }

    #[test]
    fn projection_refuses_an_ordinary_live_host_namespace() {
        let process =
            Incarnation::capture(std::process::id(), rustix::process::getuid().as_raw()).unwrap();
        assert!(process.mount_namespace().is_err());
    }
}
