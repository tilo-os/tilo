//! Replacing a file's contents without a window in which a crash loses them.
//!
//! # Why one crate
//!
//! Writing a temp file and renaming it over the target is insufficient without
//! syncing the file and containing directory.
//!
//! `rename(2)` is atomic with respect to the *directory entry*: a reader sees either the old
//! file or the new one, never a half-written one — but it says nothing about whether the new
//! file's **contents** reached disk. Without an `fsync` on the temp file before the rename, a
//! power loss can leave the directory entry pointing at a file whose data blocks were never
//! written.
//!
//! On Unix, [`write`] writes and flushes a sibling file, renames it, and flushes
//! the held containing directory. On admitted local NTFS, it creates a native
//! write-through sibling, flushes it, renames through the held parent using
//! extended POSIX semantics, then flushes the same retained writable file.
//!
//! The Windows sequence composes documented NTFS metadata behavior; native
//! crash, operating-system and storage/filter qualification remains required.
//! Unsupported Windows volumes fail admission before file publication effects.
//! Both protocols require an existing durably provisioned parent and a trusted
//! child namespace (or caller-held exclusion against child-name mutation).
//!
//! # What it does not do
//!
//! It does not create the parent directory — callers know whether a missing parent is
//! normal (first run) or a bug, and silently creating one hides the second case. Use
//! [`write_creating_parent`] when it is the first.
//!
//! The temp file is created beside the target, never in `/tmp`, so the rename is always
//! within one filesystem — a cross-device rename fails with `EXDEV` and would leave the
//! original untouched and the write silently lost by any caller that ignored the error.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod bounded;

#[cfg(unix)]
mod existing;
pub mod lease;
#[cfg(unix)]
pub use existing::ExistingCommitError;

#[cfg(unix)]
mod provision;
#[cfg(unix)]
pub use provision::{
    DirectoryReconcileError, DirectoryState, ProvisionError, directory_state, provision_directory,
    reconcile_directory,
};

#[cfg(feature = "test-support")]
pub mod directory_probe;

#[cfg(any(unix, windows))]
mod directory;
#[cfg(any(unix, windows))]
mod identity;
#[cfg(not(any(unix, windows)))]
mod legacy;
#[cfg(any(unix, windows))]
mod native;
#[cfg(any(unix, windows))]
mod publication;
#[cfg(all(any(unix, windows), feature = "test-support"))]
pub mod test_support;

#[cfg(any(unix, windows))]
pub use directory::StorageDirectory;
#[cfg(any(unix, windows))]
pub use identity::{FileIdentity, VolumeIdentity};
#[cfg(any(unix, windows))]
pub use publication::{
    Privacy, PublicationError, PublicationState, PublishMode, publication_state, publish_if_absent,
    replace,
};

use std::fs;
use std::io;
use std::path::Path;

/// Replace `path`'s contents with `bytes`, durably. On success the file holds exactly
/// `bytes`, and a crash at any point leaves either the previous contents or the new ones.
/// The parent directory must already be durable — see [`write_creating_parent`]
/// for first-launch preparation (durable on Unix; transitional on Windows).
///
/// # Errors
///
/// Returns an error if the temp file can't be created/written/synced, or if the rename
/// over `path` fails (including `EXDEV` if `path`'s parent is on a different filesystem
/// than the temp file, or if `path`'s parent doesn't exist).
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(any(unix, windows))]
    {
        return replace(path, bytes, Privacy::Inherit).map_err(PublicationError::into_io_error);
    }
    #[cfg(not(any(unix, windows)))]
    {
        legacy::write(path, bytes)
    }
}

/// Atomically save a user document, following an existing symbolic link and
/// retaining the existing target's Unix permission bits. Other [`write`]
/// consumers retain their directory-entry replacement semantics.
///
/// Existing targets must be writable regular files. Dangling links and Unix
/// multiply-linked files fail closed rather than replacing a link or detaching
/// another name from the edited document. New paths use [`write`] defaults.
/// As with [`write`], callers must control the containing directory: resolution
/// is not a defense against concurrent hostile namespace changes. This retains
/// permission bits, not extended ACLs, ownership, or other inode metadata.
///
/// # Errors
/// Returns resolution/access errors, rejects unsupported targets, and propagates
/// [`write`] durability errors (which may occur after publication).
pub fn write_preserving_target(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return write(path, bytes),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let target = fs::canonicalize(path)?;
    let metadata = fs::metadata(&target)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "document target is not a regular file",
        ));
    }
    if metadata.permissions().readonly() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "document target is read-only",
        ));
    }
    // Do not let directory replacement bypass the target's write access. This
    // opens without truncating, so an access refusal leaves its bytes intact.
    drop(fs::OpenOptions::new().write(true).open(&target)?);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "atomic document save cannot preserve multiple hard links",
            ));
        }
        write_with_mode(&target, bytes, metadata.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    write(&target, bytes)
}

/// [`write`], creating the parent directory first when it is missing.
///
/// Unix opens/creates the entire parent chain, fsyncs every held directory
/// leaf-to-root, and publishes relative to the held leaf. Existing aliases are
/// admitted, but unresolved `missing/../other` suffixes fail before creation.
/// Windows parent creation remains transitional: file commit does not establish
/// durability of newly created ancestors.
///
/// # Errors
///
/// Returns an error if the parent directory can't be created, or any error [`write`] documents.
pub fn write_creating_parent(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        return write_provisioned(path, bytes, None);
    }
    #[cfg(windows)]
    {
        create_parent(path)?;
        return write(path, bytes);
    }
    #[cfg(not(any(unix, windows)))]
    {
        legacy::write_creating_parent(path, bytes)
    }
}

/// [`write_with_mode`], creating the parent directory first when it is
/// missing. The final file gets its restrictive mode before its atomic rename.
/// Directory creation uses ordinary inherited modes, independently of file mode.
/// Parent durability has the same platform-specific behavior as
/// [`write_creating_parent`].
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created, or any error
/// [`write_with_mode`] documents.
pub fn write_creating_parent_with_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    #[cfg(windows)]
    publication::validate_windows_mode(mode).map_err(PublicationError::into_io_error)?;
    #[cfg(unix)]
    {
        return write_provisioned(path, bytes, Some(mode));
    }
    #[cfg(windows)]
    {
        create_parent(path)?;
        return publication::replace_with_mode(path, bytes, mode)
            .map_err(PublicationError::into_io_error);
    }
    #[cfg(not(any(unix, windows)))]
    {
        legacy::write_creating_parent_with_mode(path, bytes, mode)
    }
}

#[cfg(unix)]
fn write_provisioned(path: &Path, bytes: &[u8], exact_mode: Option<u32>) -> io::Result<()> {
    let not_published =
        |source| PublicationError::new(PublicationState::NotPublished, source).into_io_error();
    let name = path.file_name().ok_or_else(|| {
        not_published(io::Error::new(
            io::ErrorKind::InvalidInput,
            "publication path has no file name",
        ))
    })?;
    native::validate_component(name).map_err(not_published)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory =
        provision_directory(parent, Privacy::Inherit).map_err(ProvisionError::into_io_error)?;
    directory
        .publish_with_mode(
            name,
            bytes,
            PublishMode::Replace,
            Privacy::Inherit,
            exact_mode,
        )
        .map_err(PublicationError::into_io_error)
}

#[cfg(windows)]
// Transitional parent preparation only. Task 4 owns ancestor durability;
// create_dir_all does not establish that the new parent is crash durable.
fn create_parent(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

/// [`write`], with unix permission bits applied to the temp file **before** it is renamed
/// into place, so the file is never visible at its final path with wider permissions than
/// intended — what a secret needs, since creating then `chmod`ing leaves a window where
/// another user can open it.
///
/// # Errors
///
/// Returns any error [`write`] documents. On Windows, only `0o600` is supported
/// and maps to [`Privacy::OwnerOnly`]; other raw modes fail before mutation.
pub fn write_with_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    #[cfg(any(unix, windows))]
    {
        return publication::replace_with_mode(path, bytes, mode)
            .map_err(PublicationError::into_io_error);
    }
    #[cfg(not(any(unix, windows)))]
    {
        legacy::write_with_mode(path, bytes, mode)
    }
}

/// Flush an existing directory after publishing or renaming a file inside it.
///
/// Windows requires a writable directory handle with backup semantics for
/// `FlushFileBuffers`; opening it with `File::open` fails before the flush.
/// Callers requiring a strict durability barrier should propagate this result.
///
/// # Errors
/// Returns the open/flush error, or `InvalidInput` if `path` is not a directory.
pub fn sync_directory(path: &Path) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        options.write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    let directory = options.open(path)?;
    if !directory.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a directory",
        ));
    }
    directory.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn directory_barrier_flushes_the_directory_and_reports_invalid_targets() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("saved.txt");
        fs::write(&file, b"saved").unwrap();
        sync_directory(root.path()).unwrap();
        assert!(sync_directory(&file).is_err());
        assert!(sync_directory(&root.path().join("missing")).is_err());
    }
    use tempfile::TempDir;

    #[test]
    fn writes_the_bytes() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("a.json");
        write(&p, b"hello").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"hello");
    }

    #[test]
    fn replaces_existing_contents_completely() {
        // Not a truncate-in-place: a shorter write must not leave a tail of the previous contents behind.
        let d = TempDir::new().unwrap();
        let p = d.path().join("a.json");
        write(&p, b"a very long previous document").unwrap();
        write(&p, b"short").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"short");
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let d = TempDir::new().unwrap();
        write(&d.path().join("a.json"), b"x").unwrap();
        let left: Vec<String> = fs::read_dir(d.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| Path::new(n).extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(left.is_empty(), "left {left:?}");
    }

    #[test]
    fn a_missing_parent_is_an_error_not_a_silent_success() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("nope").join("a.json");
        assert!(write(&p, b"x").is_err(), "must not pretend to have written");
        assert!(!p.exists());
    }

    #[test]
    fn write_creating_parent_makes_the_directory() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("deep").join("er").join("a.json");
        write_creating_parent(&p, b"x").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"x");
    }

    #[cfg(unix)]
    #[test]
    fn write_creating_parent_with_mode_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let d = TempDir::new().unwrap();
        let p = d.path().join("deep").join("vault");
        write_creating_parent_with_mode(&p, b"secret", 0o600).unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn mode_is_applied_before_the_file_is_visible() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new().unwrap();
        let p = d.path().join("vault.key");
        write_with_mode(&p, b"secret", 0o600).unwrap();
        let mode = fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a secret must land owner-only");
        assert_eq!(fs::read(&p).unwrap(), b"secret");
    }

    #[test]
    fn a_failed_rename_cleans_up_and_preserves_the_target() {
        let d = TempDir::new().unwrap();
        let target = d.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("marker"), b"original").unwrap();

        let error = write(&target, b"replacement").unwrap_err();
        #[cfg(windows)]
        assert_eq!(
            publication_state(&error),
            Some(PublicationState::PublicationUnknown)
        );
        #[cfg(not(windows))]
        let _ = error;
        assert_eq!(fs::read(target.join("marker")).unwrap(), b"original");
        let leftovers = fs::read_dir(d.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        #[cfg(not(windows))]
        assert_eq!(leftovers, 0, "failed rename must remove its temporary file");
        #[cfg(windows)]
        {
            assert_eq!(leftovers, 1, "ambiguous rename must preserve its stage");
            let stage = fs::read_dir(d.path())
                .unwrap()
                .map(Result::unwrap)
                .find(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                .unwrap();
            assert_eq!(fs::read(stage.path()).unwrap(), b"replacement");
            let typed = error
                .into_inner()
                .unwrap()
                .downcast::<PublicationError>()
                .unwrap();
            assert_eq!(
                typed.retry_commit().unwrap_err().state(),
                PublicationState::PublicationUnknown
            );
        }
    }

    #[test]
    fn concurrent_writers_publish_only_complete_values() {
        use std::sync::{Arc, Barrier};

        let d = TempDir::new().unwrap();
        let target = Arc::new(d.path().join("state"));
        let barrier = Arc::new(Barrier::new(8));
        let mut threads = Vec::new();
        for byte in 0_u8..8 {
            let target = Arc::clone(&target);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                let payload = vec![byte; 64 * 1024];
                barrier.wait();
                write(&target, &payload).unwrap();
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        let final_value = fs::read(&*target).unwrap();
        assert_eq!(final_value.len(), 64 * 1024);
        assert!(final_value.iter().all(|byte| *byte == final_value[0]));
    }

    #[test]
    fn empty_content_is_a_valid_write() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("a.json");
        write(&p, b"seed").unwrap();
        write(&p, b"").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"");
    }

    #[cfg(all(any(unix, windows), feature = "test-support"))]
    #[test]
    fn publication_commit_failure_can_be_retried_without_republishing() {
        use crate::test_support::{Checkpoint, with_failure};
        use crate::{Privacy, PublicationState};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        crate::write(&path, b"old").unwrap();
        let error = with_failure(Checkpoint::BeforeCommit, || {
            crate::replace(&path, b"new", Privacy::Inherit)
        })
        .unwrap_err();
        assert_eq!(error.state(), PublicationState::PublishedDurabilityUnknown);
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(error.retry_commit().is_ok());
    }
}

#[cfg(test)]
mod document_target_tests {
    use super::*;
    #[cfg(unix)]
    use std::path::PathBuf;

    #[test]
    fn document_save_readonly_target_refuses_without_changing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("document");
        fs::write(&path, b"original").unwrap();
        let original = fs::metadata(&path).unwrap().permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly).unwrap();
        let result = write_preserving_target(&path, b"replacement");
        fs::set_permissions(&path, original).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn document_save_follows_relative_symlink_and_preserves_private_mode() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("private");
        let link = dir.path().join("document");
        fs::write(&target, b"original").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink("private", &link).unwrap();
        write_preserving_target(&link, b"replacement").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"replacement");
        assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("private"));
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn document_save_refuses_dangling_and_multiple_hard_links() {
        use std::os::unix::fs::{MetadataExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("original");
        let hardlink = dir.path().join("hardlink");
        let dangling = dir.path().join("dangling");
        fs::write(&target, b"original").unwrap();
        fs::hard_link(&target, &hardlink).unwrap();
        symlink("missing", &dangling).unwrap();
        assert_eq!(
            write_preserving_target(&hardlink, b"replacement")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            write_preserving_target(&dangling, b"replacement")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert_eq!(fs::read(&hardlink).unwrap(), b"original");
        assert_eq!(
            fs::metadata(&target).unwrap().ino(),
            fs::metadata(&hardlink).unwrap().ino()
        );
        assert_eq!(fs::read_link(&dangling).unwrap(), PathBuf::from("missing"));
        assert!(!dir.path().join("missing").exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
    }
}
