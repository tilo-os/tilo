//! Native no-follow operations with frontend-localized refusal messages.

use cap_primitives::fs::Metadata;
use std::{fs::File, io, path::Path};

pub(super) use imager_backend::nofollow::{
    WindowsCopyBlock, metadata_nofollow, same_directory_identity,
};

fn localized(error: io::Error) -> io::Error {
    if let Some(reason) = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<WindowsCopyBlock>())
    {
        io::Error::new(error.kind(), super::strings::t(reason.message_key()))
    } else {
        error
    }
}

pub(super) fn open_directory_nofollow(
    parent: &File,
    path: &Path,
    expected: &Metadata,
) -> io::Result<File> {
    imager_backend::nofollow::open_directory_nofollow(parent, path, expected).map_err(localized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_refusals_preserve_localized_guidance() {
        let message =
            |reason| localized(io::Error::new(io::ErrorKind::InvalidData, reason)).to_string();
        let cloud = message(WindowsCopyBlock::CloudUnavailable);
        let link = message(WindowsCopyBlock::UnsupportedReparsePoint);
        assert!(cloud.contains("will not download"), "{cloud}");
        assert!(cloud.contains("more disk space"), "{cloud}");
        assert!(link.contains("even if its contents are local"), "{link}");
        assert_ne!(cloud, link);
    }
}
