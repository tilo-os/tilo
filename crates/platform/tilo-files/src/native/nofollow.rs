//! Opening a path without following a symbolic link anywhere along it, for
//! inputs another user or process may have planted links in.
//!
//! Linux resolves the whole path in one `openat2` call. Where that call is
//! missing (kernels before 5.6, or seccomp filters such as systemd's
//! `RestrictSUIDSGID=` that answer it with `ENOSYS`) and on other Unixes, the
//! path is walked one component at a time with `O_NOFOLLOW`, which refuses a
//! link at every step the same way. Other targets have no such open and fail.

use std::fs::File;
use std::io;
use std::path::Path;

/// Open `path` read-only and non-blocking, refusing a symbolic link at any
/// component, the file itself included. The file may be of any type; callers
/// expecting a regular file check its metadata.
///
/// # Errors
/// `ELOOP` or `ENOTDIR` for a link along the path, and the open's error
/// otherwise. Targets without `O_NOFOLLOW` return
/// [`io::ErrorKind::Unsupported`].
pub fn open_read(path: &Path) -> io::Result<File> {
    imp::open_read(path)
}

/// Open `relative` beneath the directory `root`, refusing a symbolic link at
/// any component and any path that is empty, absolute or contains `.` or
/// `..`. Opens a directory when `directory` is set, else the file read-only.
///
/// # Errors
/// [`io::ErrorKind::InvalidInput`] for an unsafe path, `ELOOP` or `ENOTDIR`
/// for a link along it, and the open's error otherwise. Targets without
/// `O_NOFOLLOW` return [`io::ErrorKind::Unsupported`].
pub fn open_beneath(root: &File, relative: &Path, directory: bool) -> io::Result<File> {
    use std::path::Component;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path must be relative and name only plain components",
        ));
    }
    imp::open_beneath(root, relative, directory)
}

#[cfg(unix)]
mod imp {
    use rustix::fs::{CWD, Mode, OFlags};
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    /// What a directory along a walked path is opened with: only enough to
    /// resolve the next component.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const WALK: OFlags = OFlags::PATH;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const WALK: OFlags = OFlags::RDONLY;

    fn final_flags(directory: bool) -> OFlags {
        let kind = if directory {
            OFlags::DIRECTORY
        } else {
            OFlags::NONBLOCK
        };
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | kind
    }

    pub(super) fn open_read(path: &Path) -> io::Result<File> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        match openat2(CWD, path, false, false) {
            Err(error) if error.raw_os_error() == Some(libc::ENOSYS) => {}
            opened => return opened,
        }
        let bytes = path.as_os_str().as_bytes();
        let start = if bytes.starts_with(b"/") {
            Some(rustix::fs::open(
                "/",
                WALK | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )?)
        } else {
            None
        };
        walk(start, bytes, false)
    }

    pub(super) fn open_beneath(root: &File, relative: &Path, directory: bool) -> io::Result<File> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        match openat2(root, relative, directory, true) {
            Err(error) if error.raw_os_error() == Some(libc::ENOSYS) => {}
            opened => return opened,
        }
        let start = root.as_fd().try_clone_to_owned()?;
        walk(Some(start), relative.as_os_str().as_bytes(), directory)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn openat2(dir: impl AsFd, path: &Path, directory: bool, beneath: bool) -> io::Result<File> {
        use rustix::fs::ResolveFlags;
        let mut resolve = ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS;
        if beneath {
            resolve |= ResolveFlags::BENEATH;
        }
        Ok(rustix::fs::openat2(dir, path, final_flags(directory), Mode::empty(), resolve)?.into())
    }

    /// `openat` each component from `start` (the working directory when
    /// `None`): directories with `O_NOFOLLOW | O_DIRECTORY`, where a link
    /// fails with `ENOTDIR`, and the last with `O_NOFOLLOW`, where it fails
    /// with `ELOOP`.
    pub(super) fn walk(start: Option<OwnedFd>, path: &[u8], directory: bool) -> io::Result<File> {
        let mut components = path
            .split(|byte| *byte == b'/')
            .filter(|component| !component.is_empty() && *component != b".")
            .peekable();
        let mut current = start;
        while let Some(component) = components.next() {
            let name = std::ffi::OsStr::from_bytes(component);
            let dir = current.as_ref().map_or(CWD, AsFd::as_fd);
            if components.peek().is_none() {
                return Ok(
                    rustix::fs::openat(dir, name, final_flags(directory), Mode::empty())?.into(),
                );
            }
            current = Some(rustix::fs::openat(
                dir,
                name,
                WALK | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?);
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path names no file",
        ))
    }
}

#[cfg(not(unix))]
mod imp {
    use std::fs::File;
    use std::io;
    use std::path::Path;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "opening without following symbolic links needs O_NOFOLLOW",
        )
    }

    pub(super) fn open_read(_: &Path) -> io::Result<File> {
        Err(unsupported())
    }

    pub(super) fn open_beneath(_: &File, _: &Path, _: bool) -> io::Result<File> {
        Err(unsupported())
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// `real/file` holding "bytes", `linked` linking to `real` and
    /// `real/alias` linking to `real/file`.
    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        std::fs::create_dir(path.join("real")).unwrap();
        std::fs::write(path.join("real/file"), b"bytes").unwrap();
        symlink(path.join("real"), path.join("linked")).unwrap();
        symlink(path.join("real/file"), path.join("real/alias")).unwrap();
        dir
    }

    #[test]
    fn a_link_anywhere_along_the_path_is_refused() {
        let dir = tree();
        let path = dir.path();
        let file = open_read(&path.join("real/file")).unwrap();
        assert_eq!(io::read_to_string(file).unwrap(), "bytes");
        assert!(open_read(&path.join("linked/file")).is_err());
        assert!(open_read(&path.join("real/alias")).is_err());
    }

    #[test]
    fn beneath_a_root_only_plain_components_without_links_open() {
        let dir = tree();
        let root = File::open(dir.path()).unwrap();
        assert!(open_beneath(&root, Path::new("real/file"), false).is_ok());
        assert!(open_beneath(&root, Path::new("real"), true).is_ok());
        assert!(open_beneath(&root, Path::new("linked/file"), false).is_err());
        assert!(open_beneath(&root, Path::new("real/alias"), false).is_err());
        for unsafe_path in ["../real/file", "/etc/passwd", "./real/file", ""] {
            let error = open_beneath(&root, Path::new(unsafe_path), false).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{unsafe_path:?}");
        }
    }

    /// The fallback where `openat2` is refused refuses the same links.
    #[test]
    fn the_component_walk_refuses_links_too() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = tree();
        let root = || Some(File::open(dir.path()).unwrap().into());
        assert!(imp::walk(root(), b"real/file", false).is_ok());
        assert!(imp::walk(root(), b"real", true).is_ok());
        assert!(imp::walk(root(), b"linked/file", false).is_err());
        assert!(imp::walk(root(), b"real/alias", false).is_err());
        let absolute = dir.path().join("linked/file");
        assert!(imp::walk(None, absolute.as_os_str().as_bytes(), false).is_err());
    }
}
