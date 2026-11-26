//! Shared scanner/migration handle-opening checks.

use cap_primitives::fs::{FollowSymlinks, Metadata};
use std::{fs, path::Path};

/// Why metadata alone rules an entry out of the one-time copier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsCopyBlock {
    CloudUnavailable,
    UnsupportedReparsePoint,
}

impl WindowsCopyBlock {
    /// Frontends resolve this key in their own locale catalogue.
    pub fn message_key(self) -> &'static str {
        match self {
            Self::CloudUnavailable => "imager-folder-cloud-unavailable",
            Self::UnsupportedReparsePoint => "imager-folder-reparse-unsupported",
        }
    }
}

impl std::fmt::Display for WindowsCopyBlock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CloudUnavailable => "Windows item is offline or requires recall",
            Self::UnsupportedReparsePoint => "Windows reparse points are unsupported",
        })
    }
}

impl std::error::Error for WindowsCopyBlock {}

/// Use only attribute bits: inspecting eligibility must never read file data.
/// A cloud availability flag takes precedence over the generic reparse flag.
/// Even a locally cached reparse point is unsupported by this copier; absence
/// of recall flags does not establish that the reparse target is safe.
fn classify_windows_attributes(attributes: u32) -> Option<WindowsCopyBlock> {
    const REPARSE_POINT: u32 = 0x400;
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x0004_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    if attributes & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0 {
        Some(WindowsCopyBlock::CloudUnavailable)
    } else if attributes & REPARSE_POINT != 0 {
        Some(WindowsCopyBlock::UnsupportedReparsePoint)
    } else {
        None
    }
}

pub fn windows_copy_block(metadata: &Metadata) -> Option<WindowsCopyBlock> {
    #[cfg(windows)]
    {
        use cap_primitives::fs::MetadataExt;
        classify_windows_attributes(metadata.file_attributes())
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        classify_windows_attributes(0)
    }
}

/// # Errors
/// Rejects offline/recall entries and unsupported Windows reparse points without reading their content.
pub fn reject_windows_placeholder(metadata: &Metadata) -> std::io::Result<()> {
    match windows_copy_block(metadata) {
        Some(reason) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, reason)),
        None => Ok(()),
    }
}

/// # Errors
/// Returns the native stat error when the entry cannot be inspected relative to the held directory.
pub fn metadata_nofollow(parent: &fs::File, path: &Path) -> std::io::Result<Metadata> {
    cap_primitives::fs::stat(parent, path, FollowSymlinks::No)
}

/// # Errors
/// Rejects links, placeholders, non-directories, changed directory identity, and native open/stat failures.
pub fn open_directory_nofollow(
    parent: &fs::File,
    path: &Path,
    expected: &Metadata,
) -> std::io::Result<fs::File> {
    reject_windows_placeholder(expected)?;
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
    reject_windows_placeholder(&observed)?;
    if !same_directory_identity(expected, &observed) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "directory changed while it was being opened",
        ));
    }
    Ok(opened)
}

pub fn same_directory_identity(left: &Metadata, right: &Metadata) -> bool {
    use cap_fs_ext::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}
