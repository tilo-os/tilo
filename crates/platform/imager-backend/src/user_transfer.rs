//! Bounded, private, no-replace artifact publication for native transfer actors.
use crate::{
    destination_security,
    migration::{OwnedTemporary, PinnedDirectory},
    migration_durability as durability,
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use std::{
    ffi::OsStr,
    io::{self, Read, Write},
    path::Path,
    sync::Arc,
};

const LIMIT: u64 = 1024 * 1024;
fn failure() -> io::Error {
    io::Error::other("Transfer restart metadata is missing, changed or unsupported")
}

/// Read a bounded private artifact without following links or blocking on special files.
///
/// # Errors
/// Refuses non-private files, oversized or non-regular artifacts, and changed directories.
pub fn read_private_artifact(pin: &PinnedDirectory, name: &str) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&pin.file, Path::new(name), &options)?;
    destination_security::check_private_file(&file)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(failure());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(failure());
    }
    pin.validate_destination()?;
    Ok(bytes)
}
/// Durably publish an immutable private artifact, accepting only an identical retry.
///
/// # Errors
/// Refuses oversized data, conflicting existing artifacts, and unsafe destinations.
pub fn publish_private_artifact(
    pin: &Arc<PinnedDirectory>,
    name: &str,
    bytes: &[u8],
) -> io::Result<()> {
    if bytes.len() as u64 > LIMIT {
        return Err(failure());
    }
    let mut temporary = OwnedTemporary::create(pin, OsStr::new(name))?;
    temporary.file.write_all(bytes)?;
    durability::sync_file(&temporary.file)?;
    temporary.validate()?;
    match cap_primitives::fs::hard_link(
        &pin.file,
        Path::new(&temporary.name),
        &pin.file,
        Path::new(name),
    ) {
        Ok(()) => durability::sync_directory(&pin.file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if read_private_artifact(pin, name)? == bytes {
                Ok(())
            } else {
                Err(failure())
            }
        }
        Err(error) => Err(error),
    }
}
