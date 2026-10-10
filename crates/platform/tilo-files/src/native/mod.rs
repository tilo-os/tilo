//! Path-based operations for desktop-only code whose paths are a contract
//! with other programs: identity files, sockets' lock files, documents the
//! user named. They are the Linux and macOS backends of [`Dir`](crate::Dir).
//!
//! They compile on every target, like the rest of the crate; on the web the
//! standard library has no file system and every call returns an error.
//! Portable code opens a [`Dir`](crate::Dir) instead.

pub mod atomic;
pub mod bounded;
pub mod lock;
pub mod private;

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

#[cfg(test)]
mod tests {
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
