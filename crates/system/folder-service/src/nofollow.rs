//! Shared scanner/migration handle-opening checks.

use cap_primitives::fs::{FollowSymlinks, Metadata};
use std::{fs, path::Path};

pub(super) fn metadata_nofollow(parent: &fs::File, path: &Path) -> std::io::Result<Metadata> {
    cap_primitives::fs::stat(parent, path, FollowSymlinks::No)
}

pub(super) fn open_directory_nofollow(
    parent: &fs::File,
    path: &Path,
    expected: &Metadata,
) -> std::io::Result<fs::File> {
    if expected.is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "directory is a symbolic link",
        ));
    }
    if !expected.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not a directory",
        ));
    }

    let opened = cap_primitives::fs::open_dir_nofollow(parent, path)?;
    let observed = Metadata::from_file(&opened)?;
    if !same_directory_identity(expected, &observed) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "directory changed while it was being opened",
        ));
    }
    Ok(opened)
}

pub(super) fn same_directory_identity(left: &Metadata, right: &Metadata) -> bool {
    use cap_fs_ext::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}
