//! A standard app console owns a fresh namespace, never an ambient socket path.
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

pub(crate) fn guest_socket(instance: u64) -> String {
    format!(
        "{}/.headless-{instance}/control.sock",
        crate::appdir::SANDBOX_APP_DIR
    )
}

pub(crate) struct Namespace {
    parent: File,
    directory: File,
    name: CString,
    cleanup_on_drop: bool,
}

fn directory_at(parent: &File, name: &CString) -> io::Result<File> {
    // SAFETY: both descriptors and the NUL-terminated name stay live throughout openat.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor whose ownership is transferred exactly once.
    Ok(unsafe { File::from_raw_fd(fd) })
}

impl Namespace {
    pub(crate) fn prepare(app_dir: &Path, instance: u64) -> io::Result<Self> {
        if instance == 0 {
            return Err(io::Error::other("invalid console instance"));
        }
        let parent = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(app_dir)?;
        let metadata = parent.metadata()?;
        let owner = rustix::process::geteuid().as_raw();
        if metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
            return Err(io::Error::other(
                "console app directory is not owner controlled",
            ));
        }
        let name = CString::new(format!(".headless-{instance}")).map_err(io::Error::other)?;
        // SAFETY: parent is held and name is a single validated component.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let directory = directory_at(&parent, &name)?;
        let metadata = directory.metadata()?;
        if metadata.uid() != owner || metadata.mode() & 0o777 != 0o700 {
            return Err(io::Error::other("console namespace is not private"));
        }
        Ok(Self {
            parent,
            directory,
            name,
            cleanup_on_drop: true,
        })
    }

    /// A wait error does not establish that the app has exited. Keep that
    /// namespace rather than deleting a possibly live app's socket.
    pub(crate) fn before_spawn(&mut self) {
        self.cleanup_on_drop = false;
    }

    pub(crate) fn after_exit(&mut self) -> io::Result<()> {
        self.cleanup()?;
        self.cleanup_on_drop = false;
        Ok(())
    }

    fn cleanup(&self) -> io::Result<()> {
        let current = directory_at(&self.parent, &self.name)?;
        let expected = self.directory.metadata()?;
        let actual = current.metadata()?;
        if (actual.dev(), actual.ino(), actual.uid(), actual.mode())
            != (
                expected.dev(),
                expected.ino(),
                expected.uid(),
                expected.mode(),
            )
        {
            return Err(io::Error::other("console namespace was replaced"));
        }
        let socket = c"control.sock";
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: stat points to writable storage; the held directory and fixed name stay valid.
        let result = unsafe {
            libc::fstatat(
                self.directory.as_raw_fd(),
                socket.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            // SAFETY: successful fstatat initialized stat.
            let stat = unsafe { stat.assume_init() };
            if stat.st_uid != expected.uid() || stat.st_mode & libc::S_IFMT != libc::S_IFSOCK {
                return Err(io::Error::other(
                    "console namespace contains an unexpected entry",
                ));
            }
            // SAFETY: unlink only this fixed entry in the held directory; never follow a symlink.
            if unsafe { libc::unlinkat(self.directory.as_raw_fd(), socket.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
        } else if io::Error::last_os_error().kind() != io::ErrorKind::NotFound {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: remove only the pinned namespace, and only if empty. Unknown files prevent removal.
        if unsafe {
            libc::unlinkat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Namespace {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = self.cleanup();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn headless_namespace_is_private_fresh_and_reaped_after_exit() {
        let app = tempfile::tempdir().unwrap();
        let mut namespace = Namespace::prepare(app.path(), 42).unwrap();
        let path = app.path().join(".headless-42");
        assert_eq!(path.metadata().unwrap().mode() & 0o777, 0o700);
        assert!(Namespace::prepare(app.path(), 42).is_err());
        let socket = UnixListener::bind(path.join("control.sock")).unwrap();
        namespace.before_spawn();
        drop(socket);
        namespace.after_exit().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn headless_namespace_refuses_replacement_symlinks_and_unknown_files() {
        let app = tempfile::tempdir().unwrap();
        let mut namespace = Namespace::prepare(app.path(), 43).unwrap();
        let path = app.path().join(".headless-43");
        namespace.before_spawn();
        std::fs::rename(&path, app.path().join("held")).unwrap();
        std::os::unix::fs::symlink(app.path().join("held"), &path).unwrap();
        assert!(namespace.after_exit().is_err());
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let mut other = Namespace::prepare(app.path(), 44).unwrap();
        let unknown = app.path().join(".headless-44/control.sock");
        std::fs::write(&unknown, b"keep").unwrap();
        other.before_spawn();
        assert!(other.after_exit().is_err());
        assert_eq!(std::fs::read(unknown).unwrap(), b"keep");
    }
}
