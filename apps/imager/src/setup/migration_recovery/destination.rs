//! Adapter preserving destination admission before native descriptor checks.
use crate::setup::migration::PinnedDirectory;
use std::io;

pub(super) fn capacity(directory: &PinnedDirectory, needed: u64) -> io::Result<()> {
    directory.validate_destination()?;
    imager_backend::recovery::capacity::capacity(&directory.file, needed)
}

pub(super) use imager_backend::recovery::hardware::physical;
