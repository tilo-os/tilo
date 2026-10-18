//! Advisory whole-file locks with one guard type, [`FileLock`]: the native
//! backend of [`Dir::lock`](crate::Dir::lock).
//!
//! The implementation lives in the `advisory-lock` crate; this module is that
//! crate under the platform layer's path.

pub use advisory_lock::*;

/// Prepare a native lock file's parent before acquiring the lock.
///
/// On Unix, durably provision missing directories with inherited permissions:
/// otherwise lock acquisition creates them without durability barriers, hiding
/// their creation from a subsequent atomic write. Existing directories are left
/// alone. Other targets retain the lock backend's parent-creation behavior.
///
/// # Errors
///
/// On Unix, preserves provisioning errors and their directory-creation state.
/// No lock file is created when provisioning fails.
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
