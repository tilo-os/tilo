//! Crash-safe file replacement: the write path of
//! [`Dir::write_atomic`](crate::Dir::write_atomic) on Linux and macOS.
//!
//! The implementation, with its Unix and NTFS publication protocols, lives in
//! the `atomic-file` crate; this module is that crate under the platform
//! layer's path.

pub use atomic_file::*;

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
