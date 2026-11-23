//! Bounded, private, no-replace artifact publication for native transfer actors.
use crate::{
    destination_security,
    migration::{OwnedTemporary, PinnedDirectory, PublishMode},
    migration_durability as durability,
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use std::{ffi::OsStr, io, path::Path, sync::Arc};

const LIMIT: u64 = 1024 * 1024;
fn failure() -> io::Error {
    io::Error::other("Transfer restart metadata is missing, changed or unsupported")
}

/// Read a bounded private artifact without following links or blocking on special files.
pub fn read_private_artifact(pin: &PinnedDirectory, name: &str) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&pin.file, Path::new(name), &options)?;
    destination_security::check_private_file(&file)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(failure());
    }
    let bytes = crate::record::read_bounded(file, LIMIT, failure)?;
    pin.validate_destination()?;
    Ok(bytes)
}
/// Durably publish an immutable private artifact, accepting only an identical retry.
pub fn publish_private_artifact(
    pin: &Arc<PinnedDirectory>,
    name: &str,
    bytes: &[u8],
) -> io::Result<()> {
    if bytes.len() as u64 > LIMIT {
        return Err(failure());
    }
    OwnedTemporary::create(pin, OsStr::new(name))?
        .publish(bytes, PublishMode::IfAbsentOrIdentical)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                failure()
            } else {
                error
            }
        })
}
/// Open an owner-only private workspace without following links.
pub fn private_workspace(path: &Path) -> io::Result<Arc<PinnedDirectory>> {
    let pin = PinnedDirectory::open_absolute(path)?;
    pin.validate_destination()?;
    destination_security::check_private_file(&pin.file)?;
    Ok(pin)
}
/// Remove whatever an interrupted attempt left in a private workspace.
pub fn clear_workspace(pin: &Arc<PinnedDirectory>, path: &Path) -> io::Result<()> {
    let directory = cap_std::fs::Dir::from_std_file(durability::readable_directory(&pin.file)?);
    for entry in directory.entries()? {
        let entry = entry?;
        let child = path.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(child)?;
        } else {
            std::fs::remove_file(child)?;
        }
    }
    pin.validate_destination()
}
