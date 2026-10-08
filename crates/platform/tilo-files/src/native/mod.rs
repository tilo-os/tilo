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
