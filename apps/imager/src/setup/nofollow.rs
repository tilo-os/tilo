//! Shared scanner/migration handle-opening checks.

use cap_primitives::fs::{FollowSymlinks, Metadata};
use std::{fs, path::Path};

/// Why metadata alone rules an entry out of the one-time copier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WindowsCopyBlock {
    CloudUnavailable,
    UnsupportedReparsePoint,
}

impl WindowsCopyBlock {
    pub(super) fn message(self) -> String {
        super::strings::t(match self {
            Self::CloudUnavailable => "imager-folder-cloud-unavailable",
            Self::UnsupportedReparsePoint => "imager-folder-reparse-unsupported",
        })
    }
}

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

pub(super) fn windows_copy_block(metadata: &Metadata) -> Option<WindowsCopyBlock> {
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

pub(super) fn reject_windows_placeholder(metadata: &Metadata) -> std::io::Result<()> {
    match windows_copy_block(metadata) {
        Some(reason) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            reason.message(),
        )),
        None => Ok(()),
    }
}

pub(super) fn metadata_nofollow(parent: &fs::File, path: &Path) -> std::io::Result<Metadata> {
    cap_primitives::fs::stat(parent, path, FollowSymlinks::No)
}

pub(super) fn open_directory_nofollow(
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

pub(super) fn same_directory_identity(left: &Metadata, right: &Metadata) -> bool {
    use cap_fs_ext::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_attribute_policy_is_conservative_and_does_not_conflate_cloud_with_links() {
        // Ordinary local files/directories, including sparse and pinned files,
        // are not rejected merely because storage-related flags are present.
        for local in [0, 0x10, 0x20, 0x100, 0x200, 0x80000, 0x100000] {
            assert_eq!(classify_windows_attributes(local), None, "{local:#x}");
        }
        for local in [0, 0x10, 0x20, 0x80000, 0x100000] {
            assert_eq!(
                classify_windows_attributes(local | 0x400),
                Some(WindowsCopyBlock::UnsupportedReparsePoint),
                "a locally cached or pinned reparse point is still unsupported"
            );
        }
        // All combinations of the three unavailable/recall flags must be
        // rejected, on files AND directories, with or without reparse/pinned.
        for combination in 1..8 {
            let mut flags = 0;
            for (index, flag) in [0x1000, 0x40000, 0x400000].into_iter().enumerate() {
                if combination & (1 << index) != 0 {
                    flags |= flag;
                }
            }
            for extra in [0, 0x10, 0x400, 0x410, 0x80000, 0x80410] {
                assert_eq!(
                    classify_windows_attributes(flags | extra),
                    Some(WindowsCopyBlock::CloudUnavailable),
                    "{:#x}",
                    flags | extra
                );
            }
        }
        let cloud = WindowsCopyBlock::CloudUnavailable.message();
        let link = WindowsCopyBlock::UnsupportedReparsePoint.message();
        assert!(cloud.contains("will not download"), "{cloud}");
        assert!(cloud.contains("more disk space"), "{cloud}");
        assert!(link.contains("even if its contents are local"), "{link}");
        assert_ne!(cloud, link);
    }
}
