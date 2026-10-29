//! Adapter preserving destination admission before native descriptor checks.
use imager_backend::migration::PinnedDirectory;
use std::io;

pub(super) fn capacity(directory: &PinnedDirectory, needed: u64) -> io::Result<()> {
    directory.validate_destination()?;
    imager_backend::recovery::capacity::capacity(&directory.file, needed)
}
