//! Native selected-file and private-directory custody for installer preparation.
use crate::{migration::PinnedDirectory, migration_durability};
use std::{io, path::Path, sync::Arc};

pub fn ensure_private_tree(path: &std::path::Path) -> std::io::Result<Arc<PinnedDirectory>> {
    super::require_supported()?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            let pin = PinnedDirectory::open_absolute(path)?;
            pin.validate_destination()?;
            Ok(pin)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(error)?;
            let parent = ensure_private_tree(parent)?;
            let child = parent.create_child(path.file_name().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "Missing custody name")
            })?)?;
            migration_durability::sync_directory(&child.file)?;
            migration_durability::sync_directory(&parent.file)?;
            Ok(child)
        }
        Err(error) => Err(error),
    }
}

/// Read a bounded, unchanged nofollow selected image. Pixels are decoded by the frontend.
#[cfg(unix)]
pub fn read_avatar(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::{self, Read};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    if !path.is_absolute() {
        return Err(io::Error::other(
            "avatar requires an absolute selected file",
        ));
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let before = file.metadata()?;
    const ENCODED_LIMIT: u64 = 16 * 1024 * 1024;
    if !before.is_file() || before.len() > ENCODED_LIMIT {
        return Err(io::Error::other("avatar is not a bounded regular file"));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(ENCODED_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 > ENCODED_LIMIT
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(io::Error::other("avatar changed while reading"));
    }
    Ok(bytes)
}

/// No unprotected selected-file fallback exists on unsupported hosts.
#[cfg(not(unix))]
pub fn read_avatar(_path: &Path) -> io::Result<Vec<u8>> {
    Err(super::unsupported())
}
