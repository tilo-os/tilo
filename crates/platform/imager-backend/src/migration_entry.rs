//! Portable mounted-entry kind and filesystem-relative locator validation.
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Component, Path},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    Directory,
    File,
}

/// Validate a bounded, nonempty filesystem-relative migration locator.
pub fn relative(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || path.components().count() > 128
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "A migration home locator must be a bounded filesystem-relative path without traversal.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_locator_preserves_traversal_and_depth_bounds() {
        for path in ["", "/home", "../home", "home/../other", "./home"] {
            assert!(relative(Path::new(path)).is_err(), "{path}");
        }
        assert!(relative(Path::new("home/person")).is_ok());
        assert!(relative(Path::new(&vec!["part"; 128].join("/"))).is_ok());
        assert!(relative(Path::new(&vec!["part"; 129].join("/"))).is_err());
    }

    #[test]
    fn entry_kind_preserves_wire_spelling() {
        assert_eq!(
            serde_json::to_string(&EntryKind::Directory).unwrap(),
            "\"directory\""
        );
        assert_eq!(
            serde_json::from_str::<EntryKind>("\"file\"").unwrap(),
            EntryKind::File
        );
        assert!(serde_json::from_str::<EntryKind>("\"symlink\"").is_err());
    }
}
