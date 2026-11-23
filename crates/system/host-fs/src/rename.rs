//! Renaming without replacing an existing destination.

use std::io;
use std::path::Path;

/// Outcome of [`rename_file_noreplace`] once `dest` holds the file.
#[must_use = "the destination holds the file, but the fallback may have left the source name behind"]
#[derive(Debug)]
pub enum FileRename {
    /// `src` was moved to `dest` and the source name is gone.
    Moved,
    /// On Linux when no-replace rename is unsupported, the fallback hard-linked
    /// `src` to `dest` (so `dest` now holds the file) and then failed to unlink
    /// `src` with this error.
    SourceUnlinkFailed(io::Error),
}

/// Rename `src` to `dest`, failing with `AlreadyExists` instead of replacing
/// an existing `dest`, atomically where the host supports it.
///
/// # Errors
///
/// Returns the host's error; `Unsupported` where the host has no such rename.
pub fn rename_noreplace(src: &Path, dest: &Path) -> io::Result<()> {
    imp::rename_noreplace(src, dest)
}

/// Rename a non-directory file `src` to `dest`, failing with `AlreadyExists`
/// instead of replacing an existing `dest`.
///
/// On Linux, when `renameat2(RENAME_NOREPLACE)` answers `EINVAL`, `ENOSYS`, or
/// `EOPNOTSUPP`, this falls back to hard-linking `src` to `dest` without
/// following symbolic links (refusing an existing `dest`) and then unlinking
/// `src`. The fallback never runs for a directory: a directory returns the
/// rename error directly. On macOS and Windows this performs
/// [`rename_noreplace`].
///
/// # Errors
///
/// Returns the host's error when `dest` could not be created; `Unsupported`
/// where the host has no such rename. When the Linux fallback links `src` to
/// `dest` and only unlinking `src` fails, this returns
/// `Ok(FileRename::SourceUnlinkFailed(_))`.
pub fn rename_file_noreplace(src: &Path, dest: &Path) -> io::Result<FileRename> {
    #[cfg(target_os = "linux")]
    {
        imp::rename_file_noreplace(src, dest)
    }
    #[cfg(not(target_os = "linux"))]
    {
        imp::rename_noreplace(src, dest).map(|()| FileRename::Moved)
    }
}

/// [`rename_noreplace`] between two folders held open, for a caller that keeps
/// directory descriptors instead of paths.
///
/// # Errors
///
/// Returns the host's error; `Unsupported` where the host has no such rename.
#[cfg(unix)]
pub fn rename_noreplace_at(
    src_dir: std::os::fd::BorrowedFd<'_>,
    src: &std::ffi::OsStr,
    dest_dir: std::os::fd::BorrowedFd<'_>,
    dest: &std::ffi::OsStr,
) -> io::Result<()> {
    at::rename_noreplace_at(src_dir, src, dest_dir, dest)
}

#[cfg(unix)]
mod at {
    use std::ffi::OsStr;
    use std::io;
    use std::os::fd::BorrowedFd;

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    pub fn rename_noreplace_at(
        src_dir: BorrowedFd<'_>,
        src: &OsStr,
        dest_dir: BorrowedFd<'_>,
        dest: &OsStr,
    ) -> io::Result<()> {
        rustix::fs::renameat_with(
            src_dir,
            src,
            dest_dir,
            dest,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)
    }

    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    pub fn rename_noreplace_at(
        _src_dir: BorrowedFd<'_>,
        _src: &OsStr,
        _dest_dir: BorrowedFd<'_>,
        _dest: &OsStr,
    ) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use super::FileRename;

    pub fn rename_noreplace(src: &Path, dest: &Path) -> io::Result<()> {
        let src = std::ffi::CString::new(src.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let dest = std::ffi::CString::new(dest.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: both pointers refer to live, NUL-terminated path buffers.
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                src.as_ptr(),
                libc::AT_FDCWD,
                dest.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn rename_file_noreplace(src: &Path, dest: &Path) -> io::Result<FileRename> {
        rename_file_noreplace_with(src, dest, rename_noreplace)
    }

    pub(super) fn rename_file_noreplace_with(
        src: &Path,
        dest: &Path,
        rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
    ) -> io::Result<FileRename> {
        rename_file_noreplace_inner(src, dest, rename, link_nofollow, unlink_file)
    }

    pub(super) fn rename_file_noreplace_inner(
        src: &Path,
        dest: &Path,
        rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
        link: impl FnOnce(&Path, &Path) -> io::Result<()>,
        unlink: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<FileRename> {
        match rename(src, dest) {
            Ok(()) => Ok(FileRename::Moved),
            Err(error) if is_noreplace_unsupported(&error) => {
                // Never follow a symbolic link at `src`, and never run the hard-link
                // fallback for a directory.
                if std::fs::symlink_metadata(src)?.is_dir() {
                    return Err(error);
                }
                link(src, dest)?;
                match unlink(src) {
                    Ok(()) => Ok(FileRename::Moved),
                    Err(unlink_error) => Ok(FileRename::SourceUnlinkFailed(unlink_error)),
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn link_nofollow(src: &Path, dest: &Path) -> io::Result<()> {
        rustix::fs::linkat(
            rustix::fs::CWD,
            src,
            rustix::fs::CWD,
            dest,
            rustix::fs::AtFlags::empty(),
        )
        .map_err(io::Error::from)
    }

    pub(super) fn unlink_file(path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn is_noreplace_unsupported(error: &io::Error) -> bool {
        matches!(
            error.raw_os_error(),
            Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)
        )
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    unsafe extern "C" {
        fn renamex_np(
            from: *const libc::c_char,
            to: *const libc::c_char,
            flags: u32,
        ) -> libc::c_int;
    }
    const RENAME_EXCL: u32 = 0x0000_0004;

    pub fn rename_noreplace(src: &Path, dest: &Path) -> io::Result<()> {
        let src = std::ffi::CString::new(src.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let dest = std::ffi::CString::new(dest.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: both pointers refer to live, NUL-terminated path buffers.
        let result = unsafe { renamex_np(src.as_ptr(), dest.as_ptr(), RENAME_EXCL) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileW(existing: *const u16, new: *const u16) -> i32;
    }

    pub fn rename_noreplace(src: &Path, dest: &Path) -> io::Result<()> {
        let src: Vec<u16> = src.as_os_str().encode_wide().chain(Some(0)).collect();
        let dest: Vec<u16> = dest.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: both pointers refer to live, NUL-terminated UTF-16 path buffers.
        let result = unsafe { MoveFileW(src.as_ptr(), dest.as_ptr()) };
        if result != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use std::io;
    use std::path::Path;

    pub fn rename_noreplace(_src: &Path, _dest: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::fs;
    use std::io;
    use std::os::unix::fs::symlink;

    use super::{FileRename, imp};

    const FALLBACK_ERRNOS: [i32; 3] = [libc::ENOSYS, libc::EINVAL, libc::EOPNOTSUPP];

    #[test]
    fn fallback_moves_absent_file_on_unsupported_errnos() {
        for errno in FALLBACK_ERRNOS {
            let dir = tempfile::tempdir().unwrap();
            let src = dir.path().join("src");
            let dest = dir.path().join("dest");
            fs::write(&src, b"moved bytes").unwrap();

            let outcome = imp::rename_file_noreplace_with(&src, &dest, |_, _| {
                Err(io::Error::from_raw_os_error(errno))
            })
            .unwrap();

            assert!(matches!(outcome, FileRename::Moved), "errno {errno}");
            assert_eq!(fs::read(&dest).unwrap(), b"moved bytes");
            assert!(!src.exists(), "source must be gone for errno {errno}");
        }
    }

    #[test]
    fn fallback_refuses_existing_destination_on_unsupported_errnos() {
        for errno in FALLBACK_ERRNOS {
            let dir = tempfile::tempdir().unwrap();
            let src = dir.path().join("src");
            let dest = dir.path().join("dest");
            fs::write(&src, b"new bytes").unwrap();
            fs::write(&dest, b"existing bytes").unwrap();

            let error = imp::rename_file_noreplace_with(&src, &dest, |_, _| {
                Err(io::Error::from_raw_os_error(errno))
            })
            .unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "errno {errno}");
            assert_eq!(fs::read(&dest).unwrap(), b"existing bytes");
            assert_eq!(fs::read(&src).unwrap(), b"new bytes");
        }
    }

    #[test]
    fn fallback_refuses_directory_without_linking() {
        for errno in FALLBACK_ERRNOS {
            let dir = tempfile::tempdir().unwrap();
            let src = dir.path().join("src-dir");
            let dest = dir.path().join("dest-dir");
            fs::create_dir(&src).unwrap();
            fs::write(src.join("child"), b"keep").unwrap();

            let mut link_called = false;
            let error = imp::rename_file_noreplace_inner(
                &src,
                &dest,
                |_, _| Err(io::Error::from_raw_os_error(errno)),
                |from, to| {
                    link_called = true;
                    imp::link_nofollow(from, to)
                },
                imp::unlink_file,
            )
            .unwrap_err();

            assert!(
                !link_called,
                "link must not be tried for directory (errno {errno})"
            );
            assert_eq!(error.raw_os_error(), Some(errno));
            assert!(!dest.exists());
            assert!(src.is_dir());
        }
    }

    #[test]
    fn non_fallback_errno_is_returned_without_linking() {
        for errno in [libc::EXDEV, libc::EACCES] {
            let dir = tempfile::tempdir().unwrap();
            let src = dir.path().join("src");
            let dest = dir.path().join("dest");
            fs::write(&src, b"kept bytes").unwrap();

            let mut link_called = false;
            let error = imp::rename_file_noreplace_inner(
                &src,
                &dest,
                |_, _| Err(io::Error::from_raw_os_error(errno)),
                |from, to| {
                    link_called = true;
                    imp::link_nofollow(from, to)
                },
                imp::unlink_file,
            )
            .unwrap_err();

            assert!(!link_called, "link must not be tried for errno {errno}");
            assert_eq!(error.raw_os_error(), Some(errno));
            assert!(!dest.exists());
            assert_eq!(fs::read(&src).unwrap(), b"kept bytes");
        }
    }

    #[test]
    fn fallback_reports_source_unlink_failure_after_linking() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        fs::write(&src, b"linked bytes").unwrap();

        let outcome = imp::rename_file_noreplace_inner(
            &src,
            &dest,
            |_, _| Err(io::Error::from_raw_os_error(libc::ENOSYS)),
            imp::link_nofollow,
            |_| Err(io::Error::from_raw_os_error(libc::EACCES)),
        )
        .unwrap();

        match outcome {
            FileRename::SourceUnlinkFailed(error) => {
                assert_eq!(error.raw_os_error(), Some(libc::EACCES));
            }
            FileRename::Moved => panic!("expected SourceUnlinkFailed"),
        }
        assert_eq!(fs::read(&dest).unwrap(), b"linked bytes");
        assert!(src.exists());
    }

    #[test]
    fn fallback_does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("target-dir");
        fs::create_dir(&target_dir).unwrap();
        let src_link = dir.path().join("src-link");
        let dest_link = dir.path().join("dest-link");
        symlink(&target_dir, &src_link).unwrap();

        let outcome = imp::rename_file_noreplace_with(&src_link, &dest_link, |_, _| {
            Err(io::Error::from_raw_os_error(libc::ENOSYS))
        })
        .unwrap();
        assert!(matches!(outcome, FileRename::Moved));
        assert!(fs::symlink_metadata(&dest_link).unwrap().is_symlink());
        assert_eq!(fs::read_link(&dest_link).unwrap(), target_dir);
        assert!(fs::symlink_metadata(&src_link).is_err());

        let src_file = dir.path().join("src-file");
        let dangling_dest = dir.path().join("dangling-dest");
        fs::write(&src_file, b"payload").unwrap();
        symlink(dir.path().join("missing"), &dangling_dest).unwrap();

        let error = imp::rename_file_noreplace_with(&src_file, &dangling_dest, |_, _| {
            Err(io::Error::from_raw_os_error(libc::ENOSYS))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(fs::symlink_metadata(&dangling_dest).unwrap().is_symlink());
        assert_eq!(fs::read(&src_file).unwrap(), b"payload");
    }
}
