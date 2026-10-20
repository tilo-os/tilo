//! Path-based operations for desktop-only code whose paths are a contract
//! with other programs: identity files, sockets' lock files, documents the
//! user named. They are the Linux and macOS backends of [`Dir`](crate::Dir).
//!
//! They compile on every target, like the rest of the crate; on the web the
//! standard library has no file system and every call returns an error.
//! Portable code opens a [`Dir`](crate::Dir) instead.

pub mod atomic;
#[cfg(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
mod change_watch;
#[cfg(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
pub use change_watch::{ChangeWatch, THREAD_NAME as CHANGE_WATCH_THREAD, watch_changes};
pub mod bounded;
pub mod checked;
pub mod identity;
pub mod lock;
pub mod mapped;
pub mod migration;
pub mod nofollow;
pub mod private;
pub mod retained_home;

/// Whether the native path itself names a regular file, without following a symlink.
///
/// # Errors
/// Preserves metadata errors, including a missing path or denied access;
/// unsupported targets fail.
pub fn is_regular_file(path: &std::path::Path) -> std::io::Result<bool> {
    std::fs::symlink_metadata(path).map(|metadata| metadata.is_file())
}

/// The names of the entries directly in the native directory `dir`, in no
/// particular order.
///
/// # Errors
/// Preserves the native filesystem's error, including a missing directory
/// or denied access; an entry that cannot be read is an error too.
pub fn entry_names(dir: &std::path::Path) -> std::io::Result<Vec<std::ffi::OsString>> {
    std::fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect()
}

/// Create the parents of a native path shared with another program.
/// Unlike private-directory provisioning this retains existing modes and
/// the standard recursive creation contract.
///
/// # Errors
/// Preserves the native filesystem's error and error kind, including a
/// non-directory path component or denied access; unsupported targets fail.
pub fn create_dir_all(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

/// Watches the entries directly in the native directory `dir` — files created,
/// replaced or removed there — but not what happens below its
/// subdirectories, so a busy subtree does not wake the watcher. Paths are
/// reported relative to `dir`. Unlike [`Dir::watch`](crate::Dir::watch) on a
/// file, a file atomically replaced in `dir` keeps reporting.
///
/// # Errors
///
/// [`FsError::NotFound`](crate::FsError::NotFound) when `dir` does not exist,
/// or the platform's failure to watch it.
#[cfg(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
pub fn watch_entries(dir: &std::path::Path) -> Result<crate::Watch, crate::FsError> {
    crate::disk_watch::watch_entries(dir)
}

/// Whether a watch on the native directory `dir` ([`watch_entries`]) sees
/// every change to its entries. Not on a FUSE or network mount (a projected
/// collaborative filesystem, virtiofs, NFS, SMB, 9P, Ceph, AFS), whose
/// entries can change without passing through this kernel, and not where
/// the filesystem's type cannot be told: there a watch reports local edits
/// only, and whoever lists `dir` must still read it again for the rest.
#[cfg(all(
    feature = "watch",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
#[must_use]
pub fn watch_sees_every_change(dir: &std::path::Path) -> bool {
    crate::disk_watch::sees_every_change(dir)
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_regular_file_query_preserves_non_file_and_missing_refusals() {
        let root = tempfile::tempdir().expect("temporary native directory");
        assert!(!super::is_regular_file(root.path()).expect("directory metadata"));
        let path = root.path().join("file");
        assert_eq!(
            super::is_regular_file(&path)
                .expect_err("missing file")
                .kind(),
            std::io::ErrorKind::NotFound
        );
        std::fs::write(&path, b"held").expect("native file");
        assert!(super::is_regular_file(&path).expect("regular file metadata"));
    }

    #[cfg(unix)]
    #[test]
    fn native_regular_file_query_rejects_matching_and_dangling_symlinks() {
        let root = tempfile::tempdir().expect("temporary native directory");
        let target = root.path().join("file");
        let alias = root.path().join("alias");
        std::fs::write(&target, b"held").expect("native file");
        std::os::unix::fs::symlink(&target, &alias).expect("symlink fixture");
        assert!(!super::is_regular_file(&alias).expect("symlink metadata"));
        std::fs::remove_file(&target).expect("remove owned fixture target");
        assert!(!super::is_regular_file(&alias).expect("dangling symlink metadata"));
    }

    #[test]
    fn native_directory_creation_preserves_recursive_and_failure_contracts() {
        let root = tempfile::tempdir().expect("temporary native directory");
        let nested = root.path().join("parent/child");
        super::create_dir_all(&nested).expect("recursive creation");
        super::create_dir_all(&nested).expect("existing directory remains valid");
        assert!(nested.is_dir());
        let file = root.path().join("file");
        std::fs::write(&file, b"held").expect("native file");
        let requested = file.join("child");
        let expected = std::fs::create_dir_all(&requested).expect_err("file is not a directory");
        let actual = super::create_dir_all(&requested).expect_err("same native refusal");
        assert_eq!(actual.kind(), expected.kind());
        assert_eq!(actual.raw_os_error(), expected.raw_os_error());
        assert_eq!(std::fs::read(file).expect("original file remains"), b"held");
    }
}
