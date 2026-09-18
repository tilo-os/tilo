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
//! [`write`] does all four steps: write the temp file, `fsync` it, rename, and `fsync` the
//! containing directory so the rename itself is durable.
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

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Replace `path`'s contents with `bytes`, durably. On success the file holds exactly
/// `bytes`, and a crash at any point leaves either the previous contents or the new ones.
/// The parent directory must already exist — see [`write_creating_parent`].
///
/// # Errors
///
/// Returns an error if the temp file can't be created/written/synced, or if the rename
/// over `path` fails (including `EXDEV` if `path`'s parent is on a different filesystem
/// than the temp file, or if `path`'s parent doesn't exist).
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_inner(path, bytes, None)
}

/// [`write`], creating the parent directory first when it is missing.
///
/// # Errors
///
/// Returns an error if the parent directory can't be created, or any error [`write`] documents.
pub fn write_creating_parent(path: &Path, bytes: &[u8]) -> io::Result<()> {
    create_parent(path)?;
    write_inner(path, bytes, None)
}

/// [`write_with_mode`], creating the parent directory first when it is
/// missing. The final file gets its restrictive mode before its atomic rename.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created, or any error
/// [`write_with_mode`] documents.
pub fn write_creating_parent_with_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    create_parent(path)?;
    write_inner(path, bytes, Some(mode))
}

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
/// Returns any error [`write`] documents. `mode` is ignored (no-op) on non-Unix targets.
pub fn write_with_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    write_inner(path, bytes, Some(mode))
}

fn write_inner(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let (tmp, mut file) = create_temp(path, mode)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }

    if let Err(e) = fs::rename(&tmp, path) {
        // Don't leave litter beside the target when the rename fails.
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    // The data is durable before rename, but the new directory entry is not
    // durable until its containing directory is synced. Reporting success
    // without this step can resurrect an old rollback floor after power loss.
    // A failure happens after publication, so callers may observe the new file,
    // but they must not treat it as crash-durable and may safely retry.
    let dir = path.parent().map_or_else(
        || Path::new("."),
        |parent| {
            if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            }
        },
    );
    File::open(dir)?.sync_all()?;
    Ok(())
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

fn create_temp(path: &Path, mode: Option<u32>) -> io::Result<(PathBuf, File)> {
    for _ in 0..32 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let tmp = temp_path(path, sequence);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        match options.open(&tmp) {
            Ok(file) => {
                #[cfg(unix)]
                if let Some(mode) = mode {
                    use std::os::unix::fs::PermissionsExt;
                    // `mode()` is filtered by umask. Restore the caller's
                    // exact contract while the file still has only its hidden
                    // temporary name; the initial mode can only be stricter.
                    if let Err(error) = file.set_permissions(fs::Permissions::from_mode(mode)) {
                        drop(file);
                        let _ = fs::remove_file(&tmp);
                        return Err(error);
                    }
                }
                return Ok((tmp, file));
            }
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
    fn the_temp_file_sits_beside_the_target() {
        // A temp file in /tmp would make the rename cross a filesystem and fail with EXDEV on any
        // machine where /tmp is a separate mount.
        let t = temp_path(Path::new("/var/lib/tilo/state.automerge"), 7);
        assert_eq!(t.parent(), Some(Path::new("/var/lib/tilo")));
        assert!(
            t.file_name().unwrap().to_string_lossy().starts_with('.'),
            "temp file should be hidden: {t:?}"
        );
        assert!(
            t.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tmp")),
            "expected a .tmp extension, got {t:?}"
        );
    }

    #[test]
    fn two_writes_do_not_share_a_temp_path() {
        let a = temp_path(Path::new("/x/doc"), 1);
        let b = temp_path(Path::new("/x/doc"), 2);
        assert_ne!(a, b, "every write needs a no-clobber temporary path");
        assert!(
            a.to_string_lossy()
                .contains(&std::process::id().to_string()),
            "temp path must be process-scoped: {a:?}"
        );
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

        assert!(write(&target, b"replacement").is_err());
        assert_eq!(fs::read(target.join("marker")).unwrap(), b"original");
        let leftovers = fs::read_dir(d.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0, "failed rename must remove its temporary file");
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
}
