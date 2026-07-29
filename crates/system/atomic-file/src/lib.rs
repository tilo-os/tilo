//! Replacing a file's contents without a window in which a crash loses them.
//!
//! # Why one crate
//!
//! Twenty-two places in this tree wrote a file by the same three-step dance —
//! write a temp file, rename it over the target — and eighteen of them left
//! out the step that makes the dance work.
//!
//! `rename(2)` is atomic with respect to the *directory entry*: a reader sees
//! either the old file or the new one, never a half-written one. It says
//! nothing about whether the new file's **contents** have reached the disk.
//! Without an `fsync` on the temp file before the rename, a power loss can
//! leave the directory entry pointing at a file whose data blocks were never
//! written — which is exactly the empty-or-garbage file the temp dance was
//! adopted to prevent, now reached by a narrower but still real window. The
//! copies that omitted it were not saving a syscall; they were not getting the
//! guarantee they were written for.
//!
//! [`write`] does all four steps: write the temp file, `fsync` it, rename, and
//! `fsync` the containing directory so the rename itself is durable.
//!
//! # What it does not do
//!
//! It does not create the parent directory — callers know whether a missing
//! parent is normal (first run) or a bug, and silently creating one hides the
//! second case. Use [`write_creating_parent`] when it is the first.
//!
//! The temp file is created beside the target, never in `/tmp`, so the rename
//! is always within one filesystem. A cross-device rename fails with `EXDEV`
//! and would leave the original untouched and the write silently lost by any
//! caller that ignored the error.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Replace `path`'s contents with `bytes`, durably.
///
/// On success the file at `path` holds exactly `bytes`, and a crash at any
/// point leaves either the previous contents or the new ones. The parent
/// directory must already exist — see [`write_creating_parent`].
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_inner(path, bytes, None)
}

/// [`write`], creating the parent directory first when it is missing.
pub fn write_creating_parent(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }
    write_inner(path, bytes, None)
}

/// [`write`], with unix permission bits applied to the temp file **before** it
/// is renamed into place, so the file is never visible at its final path with
/// wider permissions than intended.
///
/// This is what a secret needs: creating the file and then `chmod`ing it
/// leaves a window in which another user can open it.
pub fn write_with_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    write_inner(path, bytes, Some(mode))
}

fn write_inner(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let tmp = temp_path(path);

    // Scoped so the handle is closed before the rename.
    {
        let mut f = File::create(&tmp)?;
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
        f.write_all(bytes)?;
        // The step the copies of this pattern kept omitting: without it the
        // rename below can be durable while these bytes are not.
        f.sync_all()?;
    }

    if let Err(e) = fs::rename(&tmp, path) {
        // Don't leave litter beside the target when the rename fails.
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    // Make the rename itself durable. Failure here is not fatal — the data is
    // already fsync'd and the entry is visible — and some filesystems refuse
    // to open a directory for this, so it is best-effort by design.
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// The temp file's path: beside the target, so the rename never crosses a
/// filesystem boundary, and prefixed with a dot so a directory listing (or a
/// `pages/*.md` glob) does not treat it as real content.
///
/// The process id keeps two processes writing the same file from clobbering
/// each other's temp file and renaming a half-written one into place.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let tmp_name = format!(".{name}.{}.tmp", std::process::id());
    match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
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
        // Not a truncate-in-place: a shorter write must not leave a tail of
        // the previous contents behind.
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
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(left.is_empty(), "left {left:?}");
    }

    #[test]
    fn the_temp_file_sits_beside_the_target() {
        // A temp file in /tmp would make the rename cross a filesystem and
        // fail with EXDEV on any machine where /tmp is a separate mount.
        let t = temp_path(Path::new("/var/lib/qos/state.automerge"));
        assert_eq!(t.parent(), Some(Path::new("/var/lib/qos")));
        assert!(
            t.file_name().unwrap().to_string_lossy().starts_with('.'),
            "temp file should be hidden: {t:?}"
        );
        assert!(t.extension().unwrap() == "tmp");
    }

    #[test]
    fn two_processes_would_not_share_a_temp_path() {
        let a = temp_path(Path::new("/x/doc"));
        assert!(
            a.to_string_lossy().contains(&std::process::id().to_string()),
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
    fn an_existing_file_survives_a_failed_write() {
        // The point of the whole dance: a write that cannot complete must
        // leave the previous contents readable.
        let d = TempDir::new().unwrap();
        let p = d.path().join("a.json");
        write(&p, b"original").unwrap();

        // A directory where the temp file wants to be makes File::create fail.
        let tmp = temp_path(&p);
        fs::create_dir(&tmp).unwrap();
        assert!(write(&p, b"replacement").is_err());
        assert_eq!(fs::read(&p).unwrap(), b"original", "old contents survive");
        fs::remove_dir(&tmp).unwrap();
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
