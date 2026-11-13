//! Mounted-media authority. Unsupported platforms refuse rather than accepting
//! arbitrary paths or treating caller-supplied metadata as a grant.
use crate::media_model::TargetDrive;
#[cfg(not(target_os = "linux"))]
use std::path::Path;
use std::{io, sync::atomic::AtomicBool};
#[cfg(target_os = "linux")]
mod native;
#[cfg(target_os = "linux")]
pub use native::{MediaFilesystemGrant, check_writable_directory};
/// Always [`io::ErrorKind::Unsupported`] on this platform.
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
    /// Always [`io::ErrorKind::Unsupported`] on this platform.
    pub fn resolve(_: &TargetDrive, _: &Path) -> io::Result<Self> {
        Err(unsupported())
    }
    /// Always [`io::ErrorKind::Unsupported`] on this platform.
    pub fn validate(&self) -> io::Result<()> {
        Err(unsupported())
    }
    pub fn root_path(&self) -> &Path {
        match self._private {}
    }
    /// Always [`io::ErrorKind::Unsupported`] on this platform.
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
/// Mounts only the reviewed medium's unique EFI filesystem, and releases only mounts it created,
/// after rechecking device and partition identity. Unsupported without the native Linux media
/// writer feature.
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

/// Delivers through the authenticated migration partition; formatting and reuse-witness policy stay
/// in the native actor. Unsupported without the native Linux media writer feature.
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
/// Reads an existing migration filesystem without formatting it, checking its partition binding and
/// stored reuse witness. Unsupported without the native Linux media writer feature.
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
