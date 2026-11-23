//! Frontend archive adapters over backend-owned media filesystem lifecycles.
use super::*;
use crate::prepared_media::filesystem;

pub fn deliver<T>(
    target: &TargetDrive,
    cancel: &AtomicBool,
    write: impl FnOnce(&MediaFilesystemGrant) -> io::Result<T>,
) -> io::Result<T> {
    filesystem::with_efi_filesystem(target, cancel, |authority| {
        write(&MediaFilesystemGrant::from_backend(authority.clone())?)
    })
}
pub fn deliver_home<T>(
    target: &TargetDrive,
    cancel: &AtomicBool,
    write: impl FnOnce(&MediaFilesystemGrant) -> io::Result<T>,
) -> io::Result<T> {
    filesystem::with_home_filesystem(target, cancel, |authority| {
        write(&MediaFilesystemGrant::from_backend(authority.clone())?)
    })
}
pub fn with_existing_home<T>(
    target: &TargetDrive,
    expected: &crate::media_layout::MediaPartitionBinding,
    cancel: &AtomicBool,
    read: impl FnOnce(&MediaFilesystemGrant) -> io::Result<T>,
) -> io::Result<T> {
    filesystem::with_existing_home(target, expected, cancel, |authority| {
        read(&MediaFilesystemGrant::from_backend(authority.clone())?)
    })
}
