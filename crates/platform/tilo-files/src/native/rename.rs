//! Path-based no-replace file renames with link-then-unlink fallback where a
//! filesystem or platform does not offer an exclusive rename.

use std::fs;
use std::io;
use std::path::Path;

const DIRECTORY_FALLBACK_UNSUPPORTED: &str =
    "this file system cannot move a directory without risking replacement";

/// Outcome of [`rename_file_noreplace`] once `dest` holds the file.
#[must_use = "the destination holds the file, but the fallback may have left the source name behind"]
#[derive(Debug)]
pub enum FileRename {
    /// `src` was moved to `dest` and the source name is gone.
    Moved,
    /// The link-then-unlink path hard-linked `src` to `dest` (so `dest` now
    /// holds the file) and then failed to unlink `src` with this error.
    SourceUnlinkFailed(io::Error),
}

/// Rename a non-directory file `src` to `dest`, failing with `AlreadyExists`
/// instead of replacing an existing `dest`.
///
/// Equal paths (`src == dest`) are refused before any system call with
/// `AlreadyExists` (carrying `EEXIST` on Unix).
///
/// On Linux and Apple systems this tries the exclusive rename
/// (`renameat2(RENAME_NOREPLACE)` / `renameatx_np(RENAME_EXCL)`) and, when the
/// kernel or filesystem answers `EINVAL`, `ENOSYS`, or `ENOTSUP`, falls back
/// to hard-linking `src` to `dest` (refusing an existing `dest`) and then
/// unlinking `src`. On every other target — Android and Windows included — it
/// takes the hard-link-then-unlink path directly.
///
/// On Unix the hard link uses `linkat` with empty flags and follows no
/// symbolic link; on other targets it uses `std::fs::hard_link`, which on
/// Windows links to a symbolic link's target.
///
/// The link-then-unlink path never runs for a directory: on Linux and Apple
/// systems a directory returns the rename error directly; on other targets it
/// returns `io::ErrorKind::Unsupported`.
///
/// # Errors
///
/// Returns the host's error when `dest` could not be created (carrying the raw
/// OS error, such as `EEXIST` on Unix when `dest` already exists or when
/// `src == dest`). When the link-then-unlink path creates `dest` and only
/// unlinking `src` fails, this returns
/// `Ok(FileRename::SourceUnlinkFailed(_))`.
pub fn rename_file_noreplace(src: &Path, dest: &Path) -> io::Result<FileRename> {
    if src == dest {
        return Err(already_exists_error());
    }
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    {
        rename_noreplace_inner(
            src,
            dest,
            |error| error,
            rename_exclusive,
            hard_link_entry,
            unlink_file,
        )
    }
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    {
        link_then_unlink_with(
            src,
            dest,
            directory_unsupported_error,
            hard_link_entry,
            unlink_file,
        )
    }
}

/// Shared no-replace rename for [`crate::disk::rename_noreplace`].
///
/// On Linux, Android, and Apple systems this tries the exclusive rename
/// (moving files and directories atomically where supported) and falls back to
/// hard-linking and unlinking on `EINVAL`, `ENOSYS`, or `ENOTSUP`; on every
/// other target it takes the link-then-unlink path directly. A directory
/// reaching the link-then-unlink path answers `Unsupported`, and a post-link
/// unlink failure is returned as an error.
pub(crate) fn rename_entry_noreplace(src: &Path, dest: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    let outcome = rename_noreplace_inner(
        src,
        dest,
        |_| directory_unsupported_error(),
        rename_exclusive,
        hard_link_entry,
        unlink_file,
    )?;
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    let outcome = link_then_unlink_with(
        src,
        dest,
        directory_unsupported_error,
        hard_link_entry,
        unlink_file,
    )?;

    match outcome {
        FileRename::Moved => Ok(()),
        FileRename::SourceUnlinkFailed(error) => Err(error),
    }
}

fn already_exists_error() -> io::Error {
    #[cfg(unix)]
    {
        io::Error::from_raw_os_error(libc::EEXIST)
    }
    #[cfg(not(unix))]
    {
        io::Error::from(io::ErrorKind::AlreadyExists)
    }
}

fn directory_unsupported_error() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, DIRECTORY_FALLBACK_UNSUPPORTED)
}

fn link_then_unlink_with(
    src: &Path,
    dest: &Path,
    directory_error: impl FnOnce() -> io::Error,
    link: impl FnOnce(&Path, &Path) -> io::Result<()>,
    unlink: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<FileRename> {
    // Do not follow a symbolic link when checking for a directory, and never
    // run the hard-link path for a directory.
    if fs::symlink_metadata(src)?.is_dir() {
        return Err(directory_error());
    }
    link(src, dest)?;
    match unlink(src) {
        Ok(()) => Ok(FileRename::Moved),
        Err(unlink_error) => Ok(FileRename::SourceUnlinkFailed(unlink_error)),
    }
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn rename_noreplace_inner(
    src: &Path,
    dest: &Path,
    on_directory: impl FnOnce(io::Error) -> io::Error,
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
    link: impl FnOnce(&Path, &Path) -> io::Result<()>,
    unlink: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<FileRename> {
    match rename(src, dest) {
        Ok(()) => Ok(FileRename::Moved),
        Err(error) if is_noreplace_unsupported(&error) => {
            link_then_unlink_with(src, dest, || on_directory(error), link, unlink)
        }
        Err(error) => Err(error),
    }
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn rename_exclusive(src: &Path, dest: &Path) -> io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        src,
        rustix::fs::CWD,
        dest,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(io::Error::from)
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn is_noreplace_unsupported(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .map(rustix::io::Errno::from_raw_os_error)
        .is_some_and(|errno| {
            matches!(
                errno,
                rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS | rustix::io::Errno::NOTSUP
            )
        })
}

fn hard_link_entry(src: &Path, dest: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        rustix::fs::linkat(
            rustix::fs::CWD,
            src,
            rustix::fs::CWD,
            dest,
            rustix::fs::AtFlags::empty(),
        )
        .map_err(io::Error::from)
    }
    #[cfg(not(unix))]
    {
        fs::hard_link(src, dest)
    }
}

fn unlink_file(path: &Path) -> io::Result<()> {
    fs::remove_file(path)
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::io;

    use super::{FileRename, rename_file_noreplace};

    #[test]
    fn public_rename_file_noreplace_moves_absent_destination() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        fs::write(&src, b"moved bytes").unwrap();

        let outcome = rename_file_noreplace(&src, &dest).unwrap();

        assert!(matches!(outcome, FileRename::Moved));
        assert_eq!(fs::read(&dest).unwrap(), b"moved bytes");
        assert!(!src.exists());
    }

    #[test]
    fn public_rename_file_noreplace_refuses_existing_destination_with_eexist() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        fs::write(&src, b"candidate bytes").unwrap();
        fs::write(&dest, b"existing bytes").unwrap();

        let error = rename_file_noreplace(&src, &dest).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
        assert_eq!(fs::read(&dest).unwrap(), b"existing bytes");
        assert_eq!(fs::read(&src).unwrap(), b"candidate bytes");
    }

    #[test]
    fn public_rename_file_noreplace_refuses_equal_paths_with_eexist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("same");
        fs::write(&path, b"kept bytes").unwrap();

        let error = rename_file_noreplace(&path, &path).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
        assert_eq!(fs::read(&path).unwrap(), b"kept bytes");
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    mod fallback {
        use std::fs;
        use std::io;
        use std::os::unix::fs::symlink;

        use super::super::{
            DIRECTORY_FALLBACK_UNSUPPORTED, FileRename, directory_unsupported_error,
            hard_link_entry, rename_noreplace_inner, unlink_file,
        };

        const FALLBACK_ERRNOS: [i32; 3] = [libc::ENOSYS, libc::EINVAL, libc::ENOTSUP];

        #[test]
        fn fallback_moves_absent_file_on_unsupported_errnos() {
            #[cfg(target_os = "linux")]
            assert_eq!(libc::ENOTSUP, libc::EOPNOTSUPP);

            for errno in FALLBACK_ERRNOS {
                let dir = tempfile::tempdir().unwrap();
                let src = dir.path().join("src");
                let dest = dir.path().join("dest");
                fs::write(&src, b"moved bytes").unwrap();

                let outcome = rename_noreplace_inner(
                    &src,
                    &dest,
                    |error| error,
                    |_, _| Err(io::Error::from_raw_os_error(errno)),
                    hard_link_entry,
                    unlink_file,
                )
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

                let error = rename_noreplace_inner(
                    &src,
                    &dest,
                    |error| error,
                    |_, _| Err(io::Error::from_raw_os_error(errno)),
                    hard_link_entry,
                    unlink_file,
                )
                .unwrap_err();

                assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "errno {errno}");
                assert_eq!(error.raw_os_error(), Some(libc::EEXIST), "errno {errno}");
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
                let error = rename_noreplace_inner(
                    &src,
                    &dest,
                    |error| error,
                    |_, _| Err(io::Error::from_raw_os_error(errno)),
                    |from, to| {
                        link_called = true;
                        hard_link_entry(from, to)
                    },
                    unlink_file,
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
        fn disk_fallback_refuses_directory_with_unsupported_without_linking() {
            for errno in FALLBACK_ERRNOS {
                let dir = tempfile::tempdir().unwrap();
                let src = dir.path().join("src-dir");
                let dest = dir.path().join("dest-dir");
                fs::create_dir(&src).unwrap();
                fs::write(src.join("child"), b"keep").unwrap();

                let mut link_called = false;
                let error = rename_noreplace_inner(
                    &src,
                    &dest,
                    |_| directory_unsupported_error(),
                    |_, _| Err(io::Error::from_raw_os_error(errno)),
                    |from, to| {
                        link_called = true;
                        hard_link_entry(from, to)
                    },
                    unlink_file,
                )
                .unwrap_err();

                assert!(
                    !link_called,
                    "link must not be tried for directory (errno {errno})"
                );
                assert_eq!(error.kind(), io::ErrorKind::Unsupported);
                assert_eq!(error.to_string(), DIRECTORY_FALLBACK_UNSUPPORTED);
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
                let error = rename_noreplace_inner(
                    &src,
                    &dest,
                    |error| error,
                    |_, _| Err(io::Error::from_raw_os_error(errno)),
                    |from, to| {
                        link_called = true;
                        hard_link_entry(from, to)
                    },
                    unlink_file,
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

            let outcome = rename_noreplace_inner(
                &src,
                &dest,
                |error| error,
                |_, _| Err(io::Error::from_raw_os_error(libc::ENOSYS)),
                hard_link_entry,
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

            let outcome = rename_noreplace_inner(
                &src_link,
                &dest_link,
                |error| error,
                |_, _| Err(io::Error::from_raw_os_error(libc::ENOSYS)),
                hard_link_entry,
                unlink_file,
            )
            .unwrap();
            assert!(matches!(outcome, FileRename::Moved));
            assert!(fs::symlink_metadata(&dest_link).unwrap().is_symlink());
            assert_eq!(fs::read_link(&dest_link).unwrap(), target_dir);
            assert!(fs::symlink_metadata(&src_link).is_err());

            let src_file = dir.path().join("src-file");
            let dangling_dest = dir.path().join("dangling-dest");
            fs::write(&src_file, b"payload").unwrap();
            symlink(dir.path().join("missing"), &dangling_dest).unwrap();

            let error = rename_noreplace_inner(
                &src_file,
                &dangling_dest,
                |error| error,
                |_, _| Err(io::Error::from_raw_os_error(libc::ENOSYS)),
                hard_link_entry,
                unlink_file,
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
            assert!(fs::symlink_metadata(&dangling_dest).unwrap().is_symlink());
            assert_eq!(fs::read(&src_file).unwrap(), b"payload");
        }
    }
}
