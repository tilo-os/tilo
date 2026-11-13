//! Advisory whole-file locks with one guard type, [`FileLock`]: the native backend of
//! [`Dir::lock`](crate::Dir::lock).

pub use advisory_lock::*;

/// Prepares a lock file's parent before the lock is taken: on Unix durably, creating missing
/// directories and syncing them leaf to root.
pub fn prepare_parent(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use super::atomic::{Privacy, ProvisionError, provision_directory};
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        if !parent.is_dir() {
            provision_directory(parent, Privacy::Inherit).map_err(ProvisionError::into_io_error)?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
