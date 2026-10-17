//! Unsupported storage backends never admit roots or create profiles.

use crate::sessions::SessionCookies;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// A root cannot be admitted without a supported storage backend.
pub struct GrantedRoot {
    _private: (),
}

impl GrantedRoot {
    /// Admit a host-selected root.
    /// # Errors
    /// Always returns `Unsupported`, without touching the filesystem.
    pub fn admit_native(_path: &Path) -> io::Result<Self> {
        Err(unavailable())
    }

    /// Bind a granted directory to its engine-visible path.
    /// # Errors
    /// Always returns `Unsupported`, without inspecting the handle or path.
    pub fn from_file(_directory: File, _path: PathBuf) -> io::Result<Self> {
        Err(unavailable())
    }
}

/// A profile cannot be prepared without a supported storage backend.
pub struct Profile {
    _private: (),
}

impl Profile {
    /// Prepare a selected website session.
    /// # Errors
    /// Always returns `Unsupported`.
    pub fn prepare(_root: &GrantedRoot, _session: &SessionCookies) -> io::Result<Self> {
        Err(unavailable())
    }

    /// Obtain the engine-visible profile path.
    /// # Errors
    /// Always returns `Unsupported`.
    pub fn path(&self) -> io::Result<&Path> {
        Err(unavailable())
    }

    /// Unsupported backends cannot import cookies.
    #[must_use]
    pub fn needs_import(&self) -> bool {
        false
    }

    /// Record completed cookie installation.
    /// # Errors
    /// Always returns `Unsupported`.
    pub fn complete(&mut self) -> io::Result<()> {
        Err(unavailable())
    }

    /// Cancel an incomplete import after engine exit.
    /// # Errors
    /// Always returns `Unsupported`.
    pub fn cancel_after_engine_exit(&mut self) -> io::Result<()> {
        Err(unavailable())
    }

    /// Fence a profile before forgetting it.
    /// # Errors
    /// Always returns `Unsupported`.
    pub fn begin_forget(&mut self) -> io::Result<()> {
        Err(unavailable())
    }

    /// Remove a fenced profile after engine exit.
    /// # Errors
    /// Always returns `Unsupported`.
    pub fn cleanup_after_engine_exit(&mut self) -> io::Result<()> {
        Err(unavailable())
    }

    /// Forget a closed website session.
    /// # Errors
    /// Always returns `Unsupported`, without touching the filesystem.
    pub fn forget_closed(_root: &GrantedRoot, _person: [u8; 32], _id: &str) -> io::Result<()> {
        Err(unavailable())
    }

    /// Retry cleanup of a fenced website session.
    /// # Errors
    /// Always returns `Unsupported`, without touching the filesystem.
    pub fn retry_forget(_root: &GrantedRoot, _person: [u8; 32], _id: &str) -> io::Result<()> {
        Err(unavailable())
    }
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Website session profiles are unavailable on this platform",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_backend_refuses_root_admission() {
        assert!(!super::super::supported());
        assert_eq!(
            GrantedRoot::admit_native(Path::new("not-an-admitted-root"))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }
}
