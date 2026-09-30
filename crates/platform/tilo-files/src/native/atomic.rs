//! Replacing a file's contents without a window in which a crash loses them:
//! the write path of [`Dir::write_atomic`](crate::Dir::write_atomic) on Linux
//! and macOS, and the path-based API for native code whose files are a
//! contract with other programs.
//!
//! # Why one module
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
//! On Unix, [`write()`] writes and flushes a sibling file, renames it, and flushes
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
//!
//! Every write gets its own exclusively created temp name (process id plus a
//! per-process sequence), so concurrent writers never truncate each other's
//! half-written temp file, and a temp file left behind by a crash never blocks
//! the next save.
//!
//! With the `json` feature, `json` adds load-or-default reading and atomic
//! saving of `serde` values on top of the same write path.

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod bounded;

#[cfg(feature = "json")]
pub mod json;

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
#[cfg(any(unix, windows))]
mod native;
#[cfg(not(any(unix, windows)))]
mod portable;
#[cfg(any(unix, windows))]
mod publication;
#[cfg(any(unix, windows))]
mod replacement_block;
#[cfg(all(any(unix, windows), feature = "test-support"))]
pub mod test_support;
#[cfg(any(unix, windows))]
pub use replacement_block::block_replacement;

/// Where the standard library has no file system (a browser page) there is
/// no file to hold back: the replacement cannot be blocked.
///
/// # Errors
///
/// Always [`std::io::ErrorKind::Unsupported`].
#[cfg(not(any(unix, windows)))]
pub fn block_replacement(
    _path: &std::path::Path,
    _release: std::sync::mpsc::Receiver<()>,
    _timeout: std::time::Duration,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this target has no file system to block a replacement on",
    ))
}

#[cfg(any(unix, windows))]
pub use directory::StorageDirectory;
#[cfg(any(unix, windows))]
pub use identity::{FileIdentity, VolumeIdentity};
#[cfg(any(unix, windows))]
pub use publication::{
    Privacy, PublicationError, PublicationState, PublishMode, publication_state, publish_if_absent,
    replace,
};

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Sequence for the hidden sibling names [`write_with_temp_opener`] and
/// [`create_with_temp_opener`] offer to caller-supplied temp openers.
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
        replace(path, bytes, Privacy::Inherit).map_err(PublicationError::into_io_error)
    }
    #[cfg(not(any(unix, windows)))]
    {
        portable::write(path, bytes)
    }
}

/// Atomically save a user document, following an existing symbolic link and
/// retaining the existing target's Unix permission bits. Other [`write()`]
/// consumers retain their directory-entry replacement semantics.
///
/// Existing targets must be writable regular files. Dangling links and Unix
/// multiply-linked files fail closed rather than replacing a link or detaching
/// another name from the edited document. New paths use [`write()`] defaults.
/// As with [`write()`], callers must control the containing directory: resolution
/// is not a defense against concurrent hostile namespace changes. This retains
/// permission bits, not extended ACLs, ownership, or other inode metadata.
///
/// # Errors
/// Returns resolution/access errors, rejects unsupported targets, and propagates
/// [`write()`] durability errors (which may occur after publication).
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

/// [`write()`], creating the parent directory first when it is missing.
///
/// Unix opens/creates the entire parent chain, fsyncs every held directory
/// leaf-to-root, and publishes relative to the held leaf. Existing aliases are
/// admitted, but unresolved `missing/../other` suffixes fail before creation.
/// Windows parent creation remains transitional: file commit does not establish
/// durability of newly created ancestors.
///
/// # Errors
///
/// Returns an error if the parent directory can't be created, or any error [`write()`] documents.
pub fn write_creating_parent(path: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        write_provisioned(path, bytes, None)
    }
    #[cfg(windows)]
    {
        create_parent(path)?;
        write(path, bytes)
    }
    #[cfg(not(any(unix, windows)))]
    {
        portable::write_creating_parent(path, bytes)
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
        write_provisioned(path, bytes, Some(mode))
    }
    #[cfg(windows)]
    {
        create_parent(path)?;
        publication::replace_with_mode(path, bytes, mode).map_err(PublicationError::into_io_error)
    }
    #[cfg(not(any(unix, windows)))]
    {
        portable::write_creating_parent_with_mode(path, bytes, mode)
    }
}

/// Save user-private state: the parent directory is created owner-only (`0700`)
/// and tightened to `0700` when it already exists, and the file lands with mode
/// `0600` before it is visible at `path` — see [`write_with_mode`].
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created or restricted, or
/// any error [`write_with_mode`] documents.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    create_private_parent(path)?;
    write_with_mode(path, bytes, 0o600)
}

/// Create `path`'s parent owner-only, and restrict it when it already exists:
/// `create_dir_all`'s mode only reaches newly created components.
fn create_private_parent(path: &Path) -> io::Result<()> {
    let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir)
}

/// [`write()`], with the temp file opened by `open_temp` instead of this module —
/// for callers that must validate the opened object (owner, access, not a link)
/// before trusting it with their bytes.
///
/// `open_temp` receives a fresh hidden sibling path and must create it
/// **exclusively** (`create_new`); an `AlreadyExists` error makes the next
/// name be tried. The file is synced, renamed over `path` and the directory
/// synced exactly as [`write()`] does.
///
/// # Errors
///
/// Returns `open_temp`'s error, or any error [`write()`] documents.
pub fn write_with_temp_opener(
    path: &Path,
    bytes: &[u8],
    open_temp: impl FnMut(&Path) -> io::Result<File>,
) -> io::Result<()> {
    publish(path, bytes, open_temp, Publish::Replace).map(|_| ())
}

/// Publish `bytes` at `path` only if nothing exists there yet, never replacing
/// a concurrently created file. The complete, synced temp file is hard-linked
/// into place, so the name never refers to partial contents. Returns `false`
/// when another writer already owns the name.
///
/// `open_temp` has the contract [`write_with_temp_opener`] documents.
///
/// # Errors
///
/// Returns creation, write, sync or hard-link errors. The filesystem must
/// support same-directory hard links for the no-clobber guarantee.
pub fn create_with_temp_opener(
    path: &Path,
    bytes: &[u8],
    open_temp: impl FnMut(&Path) -> io::Result<File>,
) -> io::Result<bool> {
    publish(path, bytes, open_temp, Publish::CreateNew)
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
// Transitional parent preparation only; ancestor durability is handled elsewhere:
// create_dir_all does not establish that the new parent is crash durable.
fn create_parent(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

/// [`write()`], with unix permission bits applied to the temp file **before** it is renamed
/// into place, so the file is never visible at its final path with wider permissions than
/// intended — what a secret needs, since creating then `chmod`ing leaves a window where
/// another user can open it.
///
/// # Errors
///
/// Returns any error [`write()`] documents. On Windows, only `0o600` is supported
/// and maps to [`Privacy::OwnerOnly`]; other raw modes fail before mutation.
pub fn write_with_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    #[cfg(any(unix, windows))]
    {
        publication::replace_with_mode(path, bytes, mode).map_err(PublicationError::into_io_error)
    }
    #[cfg(not(any(unix, windows)))]
    {
        portable::write_with_mode(path, bytes, mode)
    }
}

/// Replace `path` with `bytes` for data that can be recreated, such as a cache entry: readers
/// still see either the old file or the new one, never a partial one, but nothing is synced.
/// After a crash `path` may hold the old contents, the new ones, or a torn file, so the reader
/// must validate what it finds. Skipping the syncs keeps a write from waiting for the
/// filesystem to flush every other dirty page first.
///
/// # Errors
///
/// Returns an error if the temp file can't be created or written, or if the rename fails.
pub fn write_regenerable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    publish(
        path,
        bytes,
        |tmp| {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(tmp)
        },
        Publish::ReplaceUnsynced,
    )
    .map(|_| ())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Publish {
    /// Rename over whatever `path` currently names.
    Replace,
    /// Hard-link into place only while `path` is still absent.
    CreateNew,
    /// Rename over `path` without syncing the file or its directory.
    ReplaceUnsynced,
}

fn publish(
    path: &Path,
    bytes: &[u8],
    open: impl FnMut(&Path) -> io::Result<File>,
    how: Publish,
) -> io::Result<bool> {
    let (tmp, mut file) = create_temp(path, open)?;
    let sync = how != Publish::ReplaceUnsynced;
    let written = file
        .write_all(bytes)
        .and_then(|()| if sync { file.sync_all() } else { Ok(()) });
    drop(file);
    if let Err(error) = written {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }

    let published = match how {
        Publish::Replace | Publish::ReplaceUnsynced => fs::rename(&tmp, path),
        Publish::CreateNew => fs::hard_link(&tmp, path),
    };
    if how == Publish::CreateNew || published.is_err() {
        // Don't leave litter beside the target: after a hard link the temp
        // name is a second link, and after a failed rename it is orphaned.
        let _ = fs::remove_file(&tmp);
    }
    if let Err(error) = published {
        if how == Publish::CreateNew && error.kind() == io::ErrorKind::AlreadyExists {
            return Ok(false);
        }
        return Err(error);
    }

    // The data is durable before rename, but the new directory entry is not
    // durable until its containing directory is synced. Reporting success
    // without this step can resurrect an old rollback floor after power loss.
    // A failure happens after publication, so callers may observe the new file,
    // but they must not treat it as crash-durable and may safely retry.
    if sync {
        sync_directory(parent_or_current(path))?;
    }
    Ok(true)
}

fn parent_or_current(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
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

/// The temp file's path: beside the target so the rename never crosses a filesystem
/// boundary, dot-prefixed so a directory listing (or a `pages/*.md` glob) doesn't treat it
/// as real content, and process-id-suffixed so two processes writing the same file don't
/// clobber each other's temp file.
fn temp_path(path: &Path, sequence: u64) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "file".to_string(), |n| n.to_string_lossy().into_owned());
    let tmp_name = format!(".{name}.{}.{sequence}.tmp", std::process::id());
    match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

fn create_temp(
    path: &Path,
    mut open: impl FnMut(&Path) -> io::Result<File>,
) -> io::Result<(PathBuf, File)> {
    for _ in 0..32 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let tmp = temp_path(path, sequence);
        match open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique atomic-write temporary file",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_barriers_target_the_containing_directory() {
        // Phase `the_directory_barrier_targets_the_containing_directory`.
        {
            assert_eq!(parent_or_current(Path::new("/a/b/c")), Path::new("/a/b"));
            assert_eq!(parent_or_current(Path::new("c")), Path::new("."));
            assert_eq!(parent_or_current(Path::new("/")), Path::new("."));
        }
        // Phase `directory_barrier_flushes_the_directory_and_reports_invalid_targets`.
        {
            let root = tempfile::tempdir().unwrap();
            let file = root.path().join("saved.txt");
            fs::write(&file, b"saved").unwrap();
            sync_directory(root.path()).unwrap();
            assert!(sync_directory(&file).is_err());
            assert!(sync_directory(&root.path().join("missing")).is_err());
        }
    }
    use tempfile::TempDir;

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn writes_publish_only_complete_values_and_leave_no_temporary_files() {
        // Phase `writes_the_bytes`.
        {
            let d = TempDir::new().unwrap();
            let p = d.path().join("a.json");
            write(&p, b"hello").unwrap();
            assert_eq!(fs::read(&p).unwrap(), b"hello");
        }
        // Phase `replaces_existing_contents_completely`.
        {
            // Not a truncate-in-place: a shorter write must not leave a tail of the previous contents behind.
            let d = TempDir::new().unwrap();
            let p = d.path().join("a.json");
            write(&p, b"a very long previous document").unwrap();
            write(&p, b"short").unwrap();
            assert_eq!(fs::read(&p).unwrap(), b"short");
        }
        // Phase `regenerable_writes_replace_completely_too`.
        {
            let d = TempDir::new().unwrap();
            let p = d.path().join("a.idx");
            write_regenerable(&p, b"a very long previous entry").unwrap();
            write_regenerable(&p, b"short").unwrap();
            assert_eq!(fs::read(&p).unwrap(), b"short");
            assert_eq!(
                fs::read_dir(d.path()).unwrap().count(),
                1,
                "no temp file left"
            );
        }
        // Phase `empty_content_is_a_valid_write`.
        {
            let d = TempDir::new().unwrap();
            let p = d.path().join("a.json");
            write(&p, b"seed").unwrap();
            write(&p, b"").unwrap();
            assert_eq!(fs::read(&p).unwrap(), b"");
        }
        // Phase `leaves_no_temp_file_behind`.
        {
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
        // Phase `a_crashed_writers_temp_file_does_not_block_the_next_save`.
        {
            // A fixed `path.tmp` name opened with `create_new` would fail forever
            // after one crash; a truncating open would share it between writers.
            let d = TempDir::new().unwrap();
            let p = d.path().join("state.json");
            for sequence in 0..4 {
                fs::write(temp_path(&p, sequence), b"left by a crash").unwrap();
            }
            write(&p, b"saved").unwrap();
            assert_eq!(fs::read(&p).unwrap(), b"saved");
        }
        // Phase `concurrent_writers_publish_only_complete_values`.
        {
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
        // Phase `a_failed_rename_cleans_up_and_preserves_the_target`.
        {
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
        // Phase `a_missing_parent_is_an_error_not_a_silent_success`.
        {
            let d = TempDir::new().unwrap();
            let p = d.path().join("nope").join("a.json");
            assert!(write(&p, b"x").is_err(), "must not pretend to have written");
            assert!(!p.exists());
        }
        // Phase `write_creating_parent_makes_the_directory`.
        {
            let d = TempDir::new().unwrap();
            let p = d.path().join("deep").join("er").join("a.json");
            write_creating_parent(&p, b"x").unwrap();
            assert_eq!(fs::read(&p).unwrap(), b"x");
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_writes_apply_owner_only_modes_before_publication() {
        // Phase `write_creating_parent_with_mode_is_private`.
        {
            use std::os::unix::fs::PermissionsExt;

            let d = TempDir::new().unwrap();
            let p = d.path().join("deep").join("vault");
            write_creating_parent_with_mode(&p, b"secret", 0o600).unwrap();
            assert_eq!(
                fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Phase `mode_is_applied_before_the_file_is_visible`.
        {
            use std::os::unix::fs::PermissionsExt;
            let d = TempDir::new().unwrap();
            let p = d.path().join("vault.key");
            write_with_mode(&p, b"secret", 0o600).unwrap();
            let mode = fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "a secret must land owner-only");
            assert_eq!(fs::read(&p).unwrap(), b"secret");
        }
        // Phase `write_private_creates_and_tightens_an_owner_only_parent`.
        {
            use std::os::unix::fs::PermissionsExt;
            let d = TempDir::new().unwrap();
            let parent = d.path().join("state");
            let p = parent.join("consents.json");
            write_private(&p, b"{}").unwrap();
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&parent), 0o700);
            assert_eq!(mode(&p), 0o600);

            fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
            write_private(&p, b"{\"a\":1}").unwrap();
            assert_eq!(mode(&parent), 0o700, "an existing parent is restricted too");
            assert_eq!(fs::read(&p).unwrap(), b"{\"a\":1}");
        }
    }

    #[test]
    fn custom_temp_openers_retry_taken_names_and_never_clobber() {
        // Phase `a_custom_opener_sees_hidden_sibling_names_and_retries_taken_ones`.
        fn a_custom_opener_sees_hidden_sibling_names_and_retries_taken_ones() {
            let d = TempDir::new().unwrap();
            let p = d.path().join("identity");
            let mut offered = Vec::new();
            write_with_temp_opener(&p, b"bytes", |tmp| {
                offered.push(tmp.to_path_buf());
                if offered.len() == 1 {
                    return Err(io::Error::from(io::ErrorKind::AlreadyExists));
                }
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(tmp)
            })
            .unwrap();
            assert_eq!(offered.len(), 2);
            assert_ne!(offered[0], offered[1]);
            assert!(offered.iter().all(|tmp| tmp.parent() == Some(d.path())));
            assert_eq!(fs::read(&p).unwrap(), b"bytes");
            assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
        }
        // Phase `a_custom_opener_sees_hidden_sibling_names_and_retries_taken_ones` (defined above).
        a_custom_opener_sees_hidden_sibling_names_and_retries_taken_ones();
        // Phase `a_rejecting_opener_leaves_the_target_untouched`.
        {
            let d = TempDir::new().unwrap();
            let p = d.path().join("identity");
            fs::write(&p, b"original").unwrap();
            let error = write_with_temp_opener(&p, b"replacement", |_| {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            })
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(fs::read(&p).unwrap(), b"original");
        }
        // Phase `create_publishes_only_while_the_name_is_free`.
        {
            let d = TempDir::new().unwrap();
            let p = d.path().join("broker-identity");
            let open = |tmp: &Path| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(tmp)
            };
            assert!(create_with_temp_opener(&p, b"first", open).unwrap());
            assert!(!create_with_temp_opener(&p, b"second", open).unwrap());
            assert_eq!(fs::read(&p).unwrap(), b"first");
            assert_eq!(
                fs::read_dir(d.path()).unwrap().count(),
                1,
                "neither the winner's nor the loser's temp file may remain"
            );
        }
    }

    #[cfg(all(any(unix, windows), feature = "test-support"))]
    #[test]
    fn publication_commit_failure_can_be_retried_without_republishing() {
        use crate::native::atomic::test_support::{Checkpoint, with_failure};
        use crate::native::atomic::{Privacy, PublicationState};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        crate::native::atomic::write(&path, b"old").unwrap();
        let error = with_failure(Checkpoint::BeforeCommit, || {
            crate::native::atomic::replace(&path, b"new", Privacy::Inherit)
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
    fn document_save_follows_symlinks_but_refuses_dangling_and_hard_links() {
        // Phase `document_save_follows_relative_symlink_and_preserves_private_mode`.
        {
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
        // Phase `document_save_refuses_dangling_and_multiple_hard_links`.
        {
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
}
