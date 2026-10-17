//! Mounted-media authority. Unsupported platforms refuse rather than accepting
//! arbitrary paths or treating caller-supplied metadata as a grant.
use crate::media_model::TargetDrive;
#[cfg(not(target_os = "linux"))]
use std::path::Path;
use std::{io, sync::atomic::AtomicBool};
#[cfg(target_os = "linux")]
mod native;
#[cfg(target_os = "linux")]
mod root;
#[cfg(target_os = "linux")]
pub use native::{MediaFilesystemGrant, check_writable_directory};
/// Check whether the descriptor's filesystem is writable on a supported host.
///
/// # Errors
/// Always returns [`io::ErrorKind::Unsupported`] on this platform.
#[cfg(not(target_os = "linux"))]
pub fn check_writable_directory(_: &std::fs::File) -> io::Result<()> {
    Err(unsupported())
}
#[cfg(not(target_os = "linux"))]
#[derive(Clone)]
pub struct MediaFilesystemGrant {
    _private: std::convert::Infallible,
}
#[cfg(not(target_os = "linux"))]
impl MediaFilesystemGrant {
    /// Resolve a mounted filesystem grant.
    ///
    /// # Errors
    /// Always returns [`io::ErrorKind::Unsupported`] on this platform.
    pub fn resolve(_: &TargetDrive, _: &Path) -> io::Result<Self> {
        Err(unsupported())
    }
    /// Revalidate a filesystem grant.
    ///
    /// # Errors
    /// Always returns [`io::ErrorKind::Unsupported`] on this platform.
    pub fn validate(&self) -> io::Result<()> {
        Err(unsupported())
    }
    pub fn root_path(&self) -> &Path {
        match self._private {}
    }
    /// Check a descriptor against the granted directory.
    ///
    /// # Errors
    /// Always returns [`io::ErrorKind::Unsupported`] on this platform.
    pub fn check_directory_identity(&self, _: &std::fs::File) -> io::Result<()> {
        Err(unsupported())
    }
}
#[cfg(not(all(target_os = "linux", feature = "flatpak-media-writer")))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Mounted prepared media requires the native Linux media writer",
    )
}
/// Mount only the reviewed medium's unique EFI filesystem. Release only mounts
/// created by this operation, after rechecking device and partition identity.
///
/// # Errors
/// Returns an error for cancellation, changed or ambiguous media/partition
/// identity, mount or grant validation failures, callback errors, or cleanup
/// failures. Returns [`io::ErrorKind::Unsupported`] without the native Linux
/// media writer feature.
pub fn with_efi_filesystem<T>(
    target: &TargetDrive,
    cancel: &AtomicBool,
    deliver: impl FnOnce(&MediaFilesystemGrant) -> io::Result<T>,
) -> io::Result<T> {
    #[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
    {
        native::automatic::deliver(target, cancel, deliver)
    }
    #[cfg(not(all(target_os = "linux", feature = "flatpak-media-writer")))]
    {
        let _ = (target, cancel, deliver);
        Err(unsupported())
    }
}

/// Deliver through the authenticated migration partition. Formatting and reuse
/// witness policy stay within the native actor, never a frontend callback.
///
/// # Errors
/// Returns an error for cancellation, failed device/GPT attestation, incompatible
/// partition geometry or filesystem, format/mount/witness failures, invalid
/// grants, callback errors, or cleanup failures. Returns
/// [`io::ErrorKind::Unsupported`] without the native Linux media writer feature.
pub fn with_home_filesystem<T>(
    target: &TargetDrive,
    cancel: &AtomicBool,
    deliver: impl FnOnce(&MediaFilesystemGrant) -> io::Result<T>,
) -> io::Result<T> {
    #[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
    {
        native::home::deliver_home(target, cancel, deliver)
    }
    #[cfg(not(all(target_os = "linux", feature = "flatpak-media-writer")))]
    {
        let _ = (target, cancel, deliver);
        Err(unsupported())
    }
}
/// Read an existing migration filesystem without formatting it, checking its
/// binding against the expected partition and its stored reuse witness.
///
/// # Errors
/// Returns an error without root privileges or an unmounted ext4 partition,
/// or for cancellation, changed device/GPT or filesystem identity, missing or
/// invalid reuse witnesses, mount or grant failures, callback errors, or cleanup
/// failures. Returns [`io::ErrorKind::Unsupported`] without the
/// native Linux media writer feature.
pub fn with_existing_home<T>(
    target: &TargetDrive,
    expected: &crate::media_model::MediaPartitionBinding,
    cancel: &AtomicBool,
    read: impl FnOnce(&MediaFilesystemGrant) -> io::Result<T>,
) -> io::Result<T> {
    #[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
    {
        native::home::with_existing_home(target, expected, cancel, read)
    }
    #[cfg(not(all(target_os = "linux", feature = "flatpak-media-writer")))]
    {
        let _ = (target, expected, cancel, read);
        Err(unsupported())
    }
}
